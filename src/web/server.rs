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
    pub thumbnail_dir: PathBuf,
    pub media_root: PathBuf,
}

pub async fn run_server(
    pool: DbPool,
    thumbnail_dir: PathBuf,
    media_root: PathBuf,
    addr: SocketAddr,
) -> anyhow::Result<()> {
    let state = Arc::new(AppState {
        pool,
        thumbnail_dir: thumbnail_dir.clone(),
        media_root: media_root.clone(),
    });

    let app = Router::new()
        .merge(routes::router())
        .nest_service("/thumbnails", ServeDir::new(&thumbnail_dir))
        .nest_service("/media", ServeDir::new(&media_root))
        .with_state(state);

    tracing::info!("Starting server on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
