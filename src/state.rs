use crate::config::AppConfig;
use crate::db::JobsDb;
use crate::process::ProcessHub;
use crate::queue::WorkerPool;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub db: Arc<JobsDb>,
    pub hub: Arc<ProcessHub>,
    pub pool: Arc<WorkerPool>,
    pub ydl_version: Arc<RwLock<String>>,
    pub extractors: Arc<RwLock<Vec<String>>>,
}
