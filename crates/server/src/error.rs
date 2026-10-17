//! 错误信封(spec §8.2): 所有非 2xx → {"error","message","request_id"}。

use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use recommender_core::api::ApiErrorCode;
use serde_json::json;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub request_id: Option<String>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            request_id: None,
        }
    }
    pub fn with_request_id(mut self, id: impl Into<String>) -> Self {
        self.request_id = Some(id.into());
        self
    }
    pub fn invalid_json(msg: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            ApiErrorCode::InvalidJson.as_str(),
            msg,
        )
    }
    pub fn invalid_request_body(msg: impl Into<String>) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiErrorCode::InvalidRequestBody.as_str(),
            msg,
        )
    }
    pub fn invalid_constraints(msg: impl Into<String>) -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            ApiErrorCode::InvalidConstraints.as_str(),
            msg,
        )
    }
    pub fn upstream(msg: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_GATEWAY,
            ApiErrorCode::UpstreamError.as_str(),
            msg,
        )
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ApiErrorCode::InternalError.as_str(),
            msg,
        )
    }
    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "缺少或无效的 Authorization: Bearer <token>",
        )
    }
    pub fn request_too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            ApiErrorCode::RequestTooLarge.as_str(),
            "请求体超过 max_request_bytes",
        )
    }
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "资源不存在")
    }
    pub fn method_not_allowed() -> Self {
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "方法不允许",
        )
    }

    pub fn from_json_rejection(rej: JsonRejection) -> Self {
        match rej {
            JsonRejection::JsonSyntaxError(e) => Self::invalid_json(e.body_text()),
            JsonRejection::JsonDataError(e) => {
                // serde_path_to_error 的消息形如
                // "Failed to deserialize the JSON body into the target type: options.top_k: invalid type ...";
                // 只取路径首段做精确匹配,避免字段名/值里恰好出现 constraints、options 的误判。
                let msg = e.body_text();
                // 未知字段属于请求体 schema 错误,不是约束值错误;其路径也可能带
                // options./constraints. 前缀(如手写键 "options.x"),必须先排除。
                // 结构性标记(避免用户值里出现 "unknown field" 字样时误判)
                if msg.contains(": unknown field ") {
                    return Self::invalid_request_body(msg);
                }
                let path_head = msg
                    .split_once(": ")
                    .map(|(_, rest)| rest)
                    .unwrap_or(&msg)
                    .split(['.', '[', ':'])
                    .next()
                    .unwrap_or("");
                if path_head == "constraints" || path_head == "options" {
                    Self::invalid_constraints(msg)
                } else {
                    Self::invalid_request_body(msg)
                }
            }
            JsonRejection::MissingJsonContentType(e) => Self::invalid_json(e.body_text()),
            JsonRejection::BytesRejection(e) => {
                let msg = e.body_text();
                if msg.to_lowercase().contains("length limit") {
                    Self::request_too_large()
                } else {
                    Self::invalid_json(msg)
                }
            }
            other => Self::invalid_json(other.body_text()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let request_id = self
            .request_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        (
            self.status,
            Json(json!({
                "error": self.code,
                "message": self.message,
                "request_id": request_id,
            })),
        )
            .into_response()
    }
}
