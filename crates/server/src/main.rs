use std::path::PathBuf;
use std::sync::Arc;

use recommender_server::{app, backends};

fn configs_dir_from_args() -> PathBuf {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--configs" {
            if let Some(v) = args.next() {
                return PathBuf::from(v);
            }
        }
    }
    PathBuf::from("configs")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let configs_dir = configs_dir_from_args();
    let configs = recommender_core::config::load_configs(&configs_dir)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    let built = backends::build_backends(&configs.server.backends).map_err(anyhow::Error::msg)?;
    let renderer = Arc::new(
        recommender_core::explain::TemplateReasonRenderer::from_dir(&PathBuf::from(
            &configs.server.templates_dir,
        ))
        .map_err(|e| anyhow::anyhow!(e.to_string()))?,
    );
    let pipeline = Arc::new(recommender_core::pipeline::Pipeline::new(
        &configs,
        Arc::new(configs.catalog.clone()),
        built.default_backend(),
        renderer,
        built.default_model_hint().to_string(),
    ));

    let listen = configs.server.server.listen.clone();
    let state = app::AppState::new(configs.server.server.clone(), pipeline);
    let router = app::build_router(state);

    let listener = tokio::net::TcpListener::bind(&listen).await?;
    tracing::info!("recommender-server listening on {listen}");
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}
