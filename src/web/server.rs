use crate::db::DbPool;
use crate::web::routes;
use axum::Router;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::services::ServeDir;

#[derive(Clone)]
pub struct AppState {
    pub pool: DbPool,
    pub media_root: PathBuf,
    pub preserve_thumbnails: bool,
}

pub async fn run_server(
    pool: DbPool,
    media_root: PathBuf,
    addr: SocketAddr,
    preserve_thumbnails: bool,
) -> anyhow::Result<()> {
    let state = Arc::new(AppState {
        pool,
        media_root: media_root.clone(),
        preserve_thumbnails,
    });

    let app = Router::new()
        .merge(routes::router())
        .nest_service("/media", ServeDir::new(&media_root))
        .with_state(state);

    tracing::info!("Starting server on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
