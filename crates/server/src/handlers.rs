//! 端点实现(spec §8.1/§8.3)与鉴权中间件(spec §9.4)。

use crate::app::AppState;
use crate::error::ApiError;
use axum::extract::{ConnectInfo, FromRequest, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use recommender_core::api::{RecommendRequest, RecommendResponse};
use recommender_core::pipeline::PipelineError;
use serde::de::DeserializeOwned;
use serde_json::json;
use std::net::SocketAddr;

/// 自定义 JSON 提取器:把 axum 的拒绝映射为 spec §8.2 的错误信封。
pub struct AppJson<T>(pub T);

impl<S, T> FromRequest<S> for AppJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(AppJson(v)),
            Err(rej) => Err(ApiError::from_json_rejection(rej)),
        }
    }
}

/// 连接对端地址。axum 0.8 的 `ConnectInfo` 未实现 `OptionalFromRequestParts`,
/// 不能直接写成 `Option<ConnectInfo<_>>`;包装提取器保留“可能缺失”的语义。
///
/// 部署约束:生产必须用 `into_make_service_with_connect_info` 提供 ConnectInfo。
/// 缺失扩展(单测/特殊部署)按计划设计视为本机;若部署省略 ConnectInfo,
/// 或由同机反向代理终结连接,则等同于信任所有调用方 —— 此类部署必须配置 token。
pub struct PeerAddr(pub Option<SocketAddr>);

impl<S> FromRequestParts<S> for PeerAddr
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0),
        ))
    }
}

const BEARER_PREFIX: &str = "Bearer ";

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn bearer_ok(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix(BEARER_PREFIX))
        .is_some_and(|t| constant_time_eq(t.as_bytes(), token.as_bytes()))
}

fn token_from_env(env: &Option<String>) -> Option<String> {
    env.as_deref()
        .and_then(|name| std::env::var(name).ok())
        .filter(|v| !v.trim().is_empty())
}

fn is_loopback_peer(peer: &PeerAddr) -> bool {
    match peer.0 {
        Some(addr) => addr.ip().is_loopback(),
        // 无 ConnectInfo(单测/特殊部署)→ 视为本机;生产必须经
        // `into_make_service_with_connect_info` 注入,否则免 token 即信任所有来源。
        None => true,
    }
}

/// 调用方鉴权(spec §9.4): 配置了 token → 必须 Bearer;未配置 → 仅本机。
/// 例外:GET /debug 页面本身从本机访问时豁免(页面不含任何数据;其 fetch 仍需 token),见 spec §9.7。
pub async fn client_auth(
    State(state): State<AppState>,
    peer: PeerAddr,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let loopback = is_loopback_peer(&peer);
    if req.uri().path() == "/debug" && loopback {
        return next.run(req).await;
    }
    match token_from_env(&state.opts.client_auth_token_env) {
        Some(token) => {
            if bearer_ok(&headers, &token) {
                next.run(req).await
            } else {
                ApiError::unauthorized().into_response()
            }
        }
        None => {
            if loopback {
                next.run(req).await
            } else {
                ApiError::unauthorized().into_response()
            }
        }
    }
}

/// 管理端点鉴权(spec §8.3 /v1/catalog): 配置了 admin token → Bearer;未配置 → 仅本机。
pub async fn admin_auth(
    State(state): State<AppState>,
    peer: PeerAddr,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    match token_from_env(&state.opts.admin_token_env) {
        Some(token) => {
            if bearer_ok(&headers, &token) {
                next.run(req).await
            } else {
                ApiError::unauthorized().into_response()
            }
        }
        None => {
            if is_loopback_peer(&peer) {
                next.run(req).await
            } else {
                ApiError::unauthorized().into_response()
            }
        }
    }
}

// ---- 端点 ----

pub async fn not_found() -> ApiError {
    ApiError::not_found()
}

pub async fn method_not_allowed() -> ApiError {
    ApiError::method_not_allowed()
}

pub async fn healthz() -> Json<serde_json::Value> {
    Json(json!({ "ok": true }))
}

pub async fn readyz(State(state): State<AppState>) -> Response {
    let ok = !state.pipeline.catalog.models().is_empty();
    if ok {
        (
            StatusCode::OK,
            Json(json!({ "ok": true, "models": state.pipeline.catalog.models().len() })),
        )
            .into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "ok": false })),
        )
            .into_response()
    }
}

pub async fn catalog(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "version": state.pipeline.catalog.version(),
        "models": state.pipeline.catalog.models(),
    }))
}

pub async fn recommend(
    State(state): State<AppState>,
    AppJson(req): AppJson<RecommendRequest>,
) -> Result<Json<RecommendResponse>, ApiError> {
    let request_id = uuid::Uuid::new_v4().to_string();
    match state.pipeline.recommend(&req, &request_id).await {
        Ok(resp) => Ok(Json(resp)),
        Err(PipelineError::InvalidConstraints(msg)) => {
            Err(ApiError::invalid_constraints(msg).with_request_id(request_id))
        }
        Err(PipelineError::InvalidRequestBody(msg)) => {
            Err(ApiError::invalid_request_body(msg).with_request_id(request_id))
        }
        Err(PipelineError::Upstream(msg)) => {
            Err(ApiError::upstream(msg).with_request_id(request_id))
        }
        Err(PipelineError::Internal(msg)) => {
            Err(ApiError::internal(msg).with_request_id(request_id))
        }
    }
}
