//! Router 组装(spec §8.3): 路由 + 鉴权中间件 + 请求体上限。

use crate::handlers;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::{middleware, Router};
use recommender_core::config::ServerOpts;
use recommender_core::pipeline::Pipeline;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub opts: Arc<ServerOpts>,
    pub pipeline: Arc<Pipeline>,
}

impl AppState {
    pub fn new(opts: ServerOpts, pipeline: Arc<Pipeline>) -> Self {
        Self {
            opts: Arc::new(opts),
            pipeline,
        }
    }
}

pub fn build_router(state: AppState) -> Router {
    let client_auth = middleware::from_fn_with_state(state.clone(), handlers::client_auth);
    let admin_auth = middleware::from_fn_with_state(state.clone(), handlers::admin_auth);

    Router::new()
        .route("/healthz", get(handlers::healthz))
        .route("/readyz", get(handlers::readyz))
        .route(
            "/v1/recommend",
            post(handlers::recommend).route_layer(client_auth),
        )
        .route(
            "/v1/catalog",
            get(handlers::catalog).route_layer(admin_auth),
        )
        .fallback(handlers::not_found)
        .method_not_allowed_fallback(handlers::method_not_allowed)
        .layer(DefaultBodyLimit::max(state.opts.max_request_bytes))
        .with_state(state)
}
