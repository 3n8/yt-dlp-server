mod api;
mod config;
mod db;
mod ffmpeg;
mod files;
mod jobs;
mod process;
mod queue;
mod state;
mod ytdlp;

use crate::config::{ytdlp_bin, AppConfig};
use crate::db::JobsDb;
use crate::process::ProcessHub;
use crate::queue::{resume_pending, WorkerPool};
use crate::state::AppState;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "yt_dlp_server=info,tower_http=info".into()),
        )
        .init();

    let config = AppConfig::load().unwrap_or_else(|e| {
        eprintln!("Invalid configuration file: {e}");
        std::process::exit(1);
    });

    let db_path = PathBuf::from(&config.ydl_server.metadata_db_path);
    let db = Arc::new(JobsDb::open(&db_path).unwrap_or_else(|e| {
        eprintln!("Failed to open jobs database: {e}");
        std::process::exit(1);
    }));

    let hub = Arc::new(ProcessHub::new());
    let pool = Arc::new(WorkerPool::start(config.clone(), Arc::clone(&db), Arc::clone(&hub)));
    resume_pending(&db, &pool).await;

    let bin = ytdlp_bin();
    let version = ytdlp::version(&bin).await;
    tracing::info!("Using yt-dlp {version} ({})", bin.display());
    let extractors = ytdlp::list_extractors(&bin).await;

    let state = AppState {
        config: config.clone(),
        db,
        hub,
        pool,
        ydl_version: Arc::new(RwLock::new(version)),
        extractors: Arc::new(RwLock::new(extractors)),
    };

    let static_dir = PathBuf::from(
        std::env::var("YDL_STATIC_DIR").unwrap_or_else(|_| "/usr/lib/yt-dlp-server/static".into()),
    );

    let app = api::router(state, static_dir)
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(TraceLayer::new_for_http());

    let addr: SocketAddr = format!("{}:{}", config.ydl_server.host, config.ydl_server.port)
        .parse()
        .unwrap_or_else(|_| ([0, 0, 0, 0], 8080).into());
    tracing::info!("Listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap_or_else(|e| {
        eprintln!("bind {addr}: {e}");
        std::process::exit(1);
    });
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("Shutting down...");
}
