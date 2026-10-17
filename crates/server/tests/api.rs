//! 路由层集成测试(spec §8.1/§8.2/§8.3): 健康检查、请求校验、错误信封、鉴权、体积上限。

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use recommender_core::config::{load_configs, ServerOpts};
use recommender_core::explain::TemplateReasonRenderer;
use recommender_core::mock::MockBackend;
use recommender_core::pipeline::Pipeline;
use recommender_server::app::{build_router, AppState};
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt; // oneshot

/// 环境变量是进程级全局资源:鉴权用例用此锁串行化 set_var/remove_var。
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn repo_path(rel: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

/// 用仓库 configs + MockBackend 组装测试 router;override 回调可改 ServerOpts(鉴权/体积上限/调试页开关)。
fn test_router(override_opts: impl FnOnce(&mut ServerOpts)) -> Router {
    let mut configs = load_configs(&repo_path("configs")).expect("仓库 configs 可加载");
    // 不受宿主环境里同名变量影响:鉴权用例会显式设置唯一 env 名。
    configs.server.server.client_auth_token_env = None;
    configs.server.server.admin_token_env = None;
    override_opts(&mut configs.server.server);
    let renderer = Arc::new(TemplateReasonRenderer::from_dir(&repo_path("templates")).unwrap());
    let pipeline = Arc::new(Pipeline::new(
        &configs,
        Arc::new(configs.catalog.clone()),
        Arc::new(MockBackend::new("mock")),
        renderer,
        "mock-1".to_string(),
    ));
    build_router(AppState::new(configs.server.server.clone(), pipeline))
}

/// 发一个请求并取 (status, body 的 JSON 解析结果)。
async fn call(app: Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// 发一个请求并取 (status, headers, body 原文)。HTML 页面不能用 JSON 解析。
async fn call_raw(app: Router, req: Request<Body>) -> (StatusCode, HeaderMap, String) {
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
}

fn json_req(
    method: &str,
    path: &str,
    body: &serde_json::Value,
    with_content_type: bool,
) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(path);
    if with_content_type {
        b = b.header("content-type", "application/json");
    }
    b.body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

fn get_req(path: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .unwrap()
}

/// 注入 ConnectInfo 扩展,模拟真实 serve 时的对端地址(oneshot 不会自动加)。
fn with_peer(mut req: Request<Body>, ip: [u8; 4]) -> Request<Body> {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((ip, 0))));
    req
}

fn recommend_req(body: &serde_json::Value) -> Request<Body> {
    json_req("POST", "/v1/recommend", body, true)
}

fn minimal_body() -> serde_json::Value {
    serde_json::json!({"request": {"messages": [{"role": "user", "content": "hi"}]}})
}

fn unique_env(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().as_simple())
}

#[tokio::test]
async fn health_and_ready() {
    let (status, body) = call(test_router(|_| {}), get_req("/healthz")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);

    let (status, body) = call(test_router(|_| {}), get_req("/readyz")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert!(body["models"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn recommend_returns_recommendations_with_meta() {
    let (status, body) = call(test_router(|_| {}), recommend_req(&minimal_body())).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let recs = body["recommendations"].as_array().expect("recommendations");
    assert!(!recs.is_empty());
    assert!(body["meta"]["request_id"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
    assert!(body["meta"]["catalog_version"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
}

#[tokio::test]
async fn missing_content_type_is_invalid_json() {
    let (status, body) = call(
        test_router(|_| {}),
        json_req("POST", "/v1/recommend", &minimal_body(), false),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_json");
    assert!(body["request_id"].as_str().is_some_and(|s| !s.is_empty()));
}

#[tokio::test]
async fn malformed_json_is_invalid_json() {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/recommend")
        .header("content-type", "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    let (status, body) = call(test_router(|_| {}), req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_json");
}

#[tokio::test]
async fn bad_option_type_is_invalid_constraints() {
    let body = serde_json::json!({
        "request": {"messages": [{"role": "user", "content": "hi"}]},
        "options": {"top_k": "abc"},
    });
    let (status, resp) = call(test_router(|_| {}), recommend_req(&body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(resp["error"], "invalid_constraints");
    assert!(
        resp["message"]
            .as_str()
            .unwrap_or_default()
            .contains("options"),
        "message: {resp}"
    );
}

/// RecommendRequest 有 deny_unknown_fields:顶层未知字段由 serde 拒绝为 JsonDataError;
/// 其路径首段不是 constraints/options,必须归为 invalid_request_body,
/// 不能被收紧后的分类器误判。
#[tokio::test]
async fn unknown_top_level_field_is_invalid_request_body() {
    let body = serde_json::json!({"request": {}, "oops": 1});
    let (status, resp) = call(test_router(|_| {}), recommend_req(&body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(resp["error"], "invalid_request_body", "body: {resp}");
}

/// 顶层未知字段名里带 `options`/`.` 字样也不能冒充 constraints 类错误。
#[tokio::test]
async fn unknown_field_containing_options_word_is_invalid_request_body() {
    let body = serde_json::json!({"request": {}, "options.x": 1});
    let (status, resp) = call(test_router(|_| {}), recommend_req(&body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(resp["error"], "invalid_request_body", "body: {resp}");
}

#[tokio::test]
async fn oversized_body_is_rejected() {
    let body = serde_json::json!({
        "request": {"messages": [{"role": "user", "content": "x".repeat(200)}]},
    });
    assert!(serde_json::to_vec(&body).unwrap().len() > 64);
    let (status, resp) = call(
        test_router(|o| o.max_request_bytes = 64),
        recommend_req(&body),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(resp["error"], "request_too_large");
}

#[tokio::test]
async fn unknown_route_uses_error_envelope() {
    let (status, body) = call(test_router(|_| {}), get_req("/nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
    assert!(body["request_id"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(body["message"].as_str().is_some_and(|s| !s.is_empty()));
}

#[tokio::test]
async fn wrong_method_uses_error_envelope() {
    let (status, body) = call(test_router(|_| {}), get_req("/v1/recommend")).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(body["error"], "method_not_allowed");
    assert!(body["request_id"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(body["message"].as_str().is_some_and(|s| !s.is_empty()));
}

#[tokio::test]
async fn loopback_peer_is_allowed_without_token() {
    let req = with_peer(recommend_req(&minimal_body()), [127, 0, 0, 1]);
    let (status, body) = call(test_router(|_| {}), req).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

#[tokio::test]
async fn remote_peer_is_denied_without_token() {
    let req = with_peer(recommend_req(&minimal_body()), [10, 0, 0, 1]);
    let (status, body) = call(test_router(|_| {}), req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "unauthorized");
}

#[tokio::test]
async fn remote_peer_is_denied_for_catalog_without_admin_token() {
    let req = with_peer(get_req("/v1/catalog"), [10, 0, 0, 1]);
    let (status, body) = call(test_router(|_| {}), req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "unauthorized");
}

#[test]
fn client_auth_requires_bearer_when_token_configured() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let env_name = unique_env("RECOMMENDER_TEST_AUTH");
        let token = "s3cret-token";
        std::env::set_var(&env_name, token);

        let opts = |o: &mut ServerOpts| o.client_auth_token_env = Some(env_name.clone());
        let body = minimal_body();

        let (status, resp) = call(test_router(opts), recommend_req(&body)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(resp["error"], "unauthorized");

        let req = Request::builder()
            .method("POST")
            .uri("/v1/recommend")
            .header("content-type", "application/json")
            .header("authorization", "Bearer wrong-token")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let (status, resp) = call(test_router(opts), req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(resp["error"], "unauthorized");

        let req = Request::builder()
            .method("POST")
            .uri("/v1/recommend")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let (status, _) = call(test_router(opts), req).await;
        assert_eq!(status, StatusCode::OK);

        std::env::remove_var(&env_name);
    });
}

#[test]
fn catalog_requires_admin_bearer_when_configured() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let env_name = unique_env("RECOMMENDER_TEST_ADMIN");
        let token = "admin-token";
        std::env::set_var(&env_name, token);

        let opts = |o: &mut ServerOpts| o.admin_token_env = Some(env_name.clone());

        let (status, resp) = call(test_router(opts), get_req("/v1/catalog")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(resp["error"], "unauthorized");

        let req = Request::builder()
            .method("GET")
            .uri("/v1/catalog")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let (status, body) = call(test_router(opts), req).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["version"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(body["models"].is_array());

        std::env::remove_var(&env_name);
    });
}

#[tokio::test]
async fn catalog_allows_loopback_without_admin_token() {
    let (status, body) = call(test_router(|_| {}), get_req("/v1/catalog")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["models"].is_array());
}

#[tokio::test]
async fn debug_page_is_served_when_enabled() {
    let (status, headers, body) = call_raw(test_router(|_| {}), get_req("/debug")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("text/html")),
        "content-type: {:?}",
        headers.get("content-type")
    );
    assert!(body.contains("Model Recommender Debug"));
    assert!(body.contains("/v1/recommend"));
}

#[tokio::test]
async fn debug_page_is_not_found_when_disabled() {
    let (status, body) = call(
        test_router(|o| o.debug_page_enabled = false),
        get_req("/debug"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}
