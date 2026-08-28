use crate::config::{ffmpeg_bin, resolve_finished_file, ytdlp_bin, AppConfig};
use crate::db::JobsDb;
use crate::ffmpeg;
use crate::jobs::{clean_logs, Job, JobStatus, JobType};
use crate::process::ProcessHub;
use crate::ytdlp;
use regex::Regex;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

const SCHEDULE_MIN_DELAY: i64 = 300;
const SCHEDULE_RELEASE_BUFFER: i64 = 60;

pub struct WorkerPool {
    pub tx: mpsc::UnboundedSender<Job>,
    pub update_lock: Arc<AtomicBool>,
}

impl WorkerPool {
    pub fn start(cfg: AppConfig, db: Arc<JobsDb>, hub: Arc<ProcessHub>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel::<Job>();
        let rx = Arc::new(tokio::sync::Mutex::new(rx));
        let workers = cfg.ydl_server.download_workers_count.max(1);
        for i in 0..workers {
            let rx = Arc::clone(&rx);
            let cfg = cfg.clone();
            let db = Arc::clone(&db);
            let hub = Arc::clone(&hub);
            tokio::spawn(async move {
                tracing::info!("Started dl worker {i}");
                loop {
                    let job = {
                        let mut g = rx.lock().await;
                        g.recv().await
                    };
                    let Some(job) = job else { break };
                    if let Err(e) = run_job(&cfg, db.clone(), &hub, job).await {
                        tracing::error!("job error: {e}");
                    }
                }
            });
        }

        let sched_db = Arc::clone(&db);
        let sched_tx = tx.clone();
        let interval = cfg.ydl_server.schedule_check_interval.max(1);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
                let now = now_ts();
                if let Ok(due) = sched_db.get_due_scheduled(now) {
                    for row in due {
                        tracing::info!("Scheduled time reached for job {}", row.id);
                        let mut job = row.into_job();
                        job.log = "Scheduled time reached".into();
                        job.status = JobStatus::Pending;
                        let _ = sched_db.update_job(&job);
                        let _ = sched_tx.send(job);
                    }
                }
            }
        });

        Self {
            tx,
            update_lock: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn enqueue(&self, job: Job) {
        let _ = self.tx.send(job);
    }
}

async fn run_job(
    cfg: &AppConfig,
    db: Arc<JobsDb>,
    hub: &Arc<ProcessHub>,
    mut job: Job,
) -> Result<(), String> {
    if let Ok(Some(row)) = db.get_job(job.id) {
        if row.status == JobStatus::Aborted {
            return Ok(());
        }
    }
    job.status = JobStatus::Running;
    let _ = db.set_status(job.id, JobStatus::Running);

    match job.job_type {
        JobType::YdlDownload => download(cfg, db.clone(), hub, &mut job).await,
        JobType::FfmpegCut => cut(cfg, db.clone(), hub, &mut job).await,
        JobType::YtdlpUpdate => update(cfg, db.clone(), hub, &mut job).await,
    }
    let _ = db.update_job(&job);
    Ok(())
}

async fn download(cfg: &AppConfig, db: Arc<JobsDb>, hub: &Arc<ProcessHub>, job: &mut Job) {
    let bin = ytdlp_bin();
    let mut extra = Vec::new();
    if job.force_generic_extractor {
        extra.push("--force-generic-extractor");
    }
    let opts = match ytdlp::merge_ydl_options(cfg, job.format.as_deref()) {
        Ok(o) => o,
        Err(e) => {
            job.status = JobStatus::Failed;
            job.log = format!("Error during download task:\n{e}");
            return;
        }
    };
    let cmd = ytdlp::build_cmd(&bin, &opts, &job.urls, &extra);

    match fetch_metadata(&bin, cfg, &job.urls, job.force_generic_extractor).await {
        Ok(metadata) => {
            let title = metadata
                .iter()
                .enumerate()
                .map(|(i, md)| {
                    md.get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or_else(|| job.urls.get(i).map(|s| s.as_str()).unwrap_or(""))
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join(", ");
            if !title.is_empty() {
                job.name = title.clone();
                let _ = db.set_name(job.id, &title);
            }

            if let Some(up) = metadata.iter().find(|md| md.get("live_status").and_then(|v| v.as_str()) == Some("is_upcoming"))
            {
                if cfg.ydl_server.schedule_upcoming {
                    if let Some(release) = up.get("release_timestamp").and_then(|v| v.as_i64()) {
                        job.log = format!("[cmd] {}", ytdlp::cmd_display(&cmd));
                        schedule_job(cfg, db.as_ref(), job, release, None);
                        return;
                    }
                }
            }

            let mut opts = opts;
            let is_playlist = metadata.first().and_then(|m| m.get("_type")).and_then(|v| v.as_str()) == Some("playlist")
                || metadata.len() > 1;
            if is_playlist {
                opts.insert(
                    "output".into(),
                    Value::String(cfg.ydl_server.output_playlist.clone()),
                );
            } else if let Some(t) = job.extra_params.get("title").and_then(|v| v.as_str()) {
                if let Some(output) = opts.get("output").and_then(|v| v.as_str()) {
                    let mut parts: Vec<&str> = output.split('/').collect();
                    if let Some(last) = parts.last_mut() {
                        *last = "";
                    }
                    let dir = parts.join("/");
                    opts.insert(
                        "output".into(),
                        Value::String(format!("{dir}/{t}.%(ext)s")),
                    );
                }
            }

            let cmd = ytdlp::build_cmd(&bin, &opts, &job.urls, &extra);
            let mut log = format!("[cmd] {}\n", ytdlp::cmd_display(&cmd));
            job.log = log.clone();
            let _ = db.set_log(job.id, &job.log);

            let args: Vec<String> = cmd.iter().skip(1).cloned().collect();
            let job_id = job.id;
            let prefix = log.clone();
            let db_log = Arc::clone(&db);
            match hub
                .run(
                    job_id,
                    &bin.display().to_string(),
                    &args,
                    move |cleaned| {
                        let _ = db_log.set_log(job_id, &format!("{prefix}{cleaned}"));
                    },
                )
                .await
            {
                Ok(0) => {
                    job.status = JobStatus::Completed;
                    if let Ok(Some(row)) = db.get_job(job.id) {
                        job.log = row.log;
                    }
                }
                Ok(rc) => {
                    job.status = JobStatus::Failed;
                    if let Ok(Some(row)) = db.get_job(job.id) {
                        job.log = row.log;
                    }
                    tracing::error!("Error in download process (RC={rc})");
                }
                Err(e) => {
                    job.status = JobStatus::Failed;
                    job.log = format!("{log}Error: {e}");
                }
            }
        }
        Err(err) => {
            let log = format!("[cmd] {}\n{err}", ytdlp::cmd_display(&cmd));
            job.log = clean_logs(&log);
            if cfg.ydl_server.schedule_upcoming {
                if let Some((release, title)) =
                    probe_upcoming(&bin, cfg, &job.urls, job.force_generic_extractor, &err).await
                {
                    schedule_job(cfg, db.as_ref(), job, release, title.as_deref());
                    return;
                }
            }
            job.status = JobStatus::Failed;
        }
    }
}

async fn fetch_metadata(
    bin: &PathBuf,
    cfg: &AppConfig,
    urls: &[String],
    force_generic: bool,
) -> Result<Vec<Value>, String> {
    let opts = cfg.ydl_options.clone();
    let mut extra = vec!["-J", "--flat-playlist"];
    if force_generic {
        extra.push("--force-generic-extractor");
    }
    let cmd = ytdlp::build_cmd(bin, &opts, urls, &extra);
    let args: Vec<String> = cmd.iter().skip(1).cloned().collect();
    let (rc, stdout, stderr) = ytdlp::run_capture(bin, &args)
        .await
        .map_err(|e| e.to_string())?;
    if rc != 0 {
        return Err(stderr);
    }
    Ok(ytdlp::parse_json_lines(&stdout))
}

async fn probe_upcoming(
    bin: &PathBuf,
    cfg: &AppConfig,
    urls: &[String],
    force_generic: bool,
    error_output: &str,
) -> Option<(i64, Option<String>)> {
    let opts = cfg.ydl_options.clone();
    let mut extra = vec!["-J", "--flat-playlist", "--ignore-no-formats-error"];
    if force_generic {
        extra.push("--force-generic-extractor");
    }
    let cmd = ytdlp::build_cmd(bin, &opts, urls, &extra);
    let args: Vec<String> = cmd.iter().skip(1).cloned().collect();
    let (rc, stdout, _) = ytdlp::run_capture(bin, &args).await.ok()?;
    if rc != 0 {
        return None;
    }
    for md in ytdlp::parse_json_lines(&stdout) {
        if md.get("live_status").and_then(|v| v.as_str()) != Some("is_upcoming") {
            continue;
        }
        let release = md
            .get("release_timestamp")
            .and_then(|v| v.as_i64())
            .or_else(|| parse_upcoming_delay(error_output));
        if let Some(release) = release {
            let title = md.get("title").and_then(|v| v.as_str()).map(|s| s.to_string());
            return Some((release, title));
        }
    }
    None
}

fn parse_upcoming_delay(output: &str) -> Option<i64> {
    let re = Regex::new(
        r"(?i)(?:will begin in|begins in|premieres in|starts in)\s+(\d+)\s+(second|minute|hour|day)s?",
    )
    .ok()?;
    let cap = re.captures(output)?;
    let n: i64 = cap.get(1)?.as_str().parse().ok()?;
    let unit = cap.get(2)?.as_str().to_ascii_lowercase();
    let secs = match unit.as_str() {
        "second" => 1,
        "minute" => 60,
        "hour" => 3600,
        "day" => 86400,
        _ => return None,
    };
    Some(now_ts() + n * secs)
}

fn schedule_job(cfg: &AppConfig, db: &JobsDb, job: &mut Job, release_ts: i64, title: Option<&str>) {
    let mut extra = job.extra_object();
    let attempts = extra
        .get("schedule_attempts")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        + 1;
    let max = cfg.ydl_server.schedule_max_attempts as u64;
    if attempts > max {
        job.log = clean_logs(&format!(
            "{}\n[scheduled] giving up after {max} attempts",
            job.log
        ));
        job.status = JobStatus::Failed;
        return;
    }
    extra.insert("schedule_attempts".into(), json!(attempts));
    job.extra_params = Value::Object(extra);
    let when = (release_ts + SCHEDULE_RELEASE_BUFFER).max(now_ts() + SCHEDULE_MIN_DELAY);
    job.scheduled_at = Some(when);
    job.status = JobStatus::Scheduled;
    let ts = chrono::DateTime::from_timestamp(when, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| when.to_string());
    job.log = clean_logs(&format!(
        "{}\n[scheduled] this event has not started yet, retrying at {ts} (attempt {attempts}/{max})",
        job.log
    ));
    if let Some(title) = title {
        job.name = title.to_string();
        let _ = db.set_name(job.id, title);
    }
    let _ = db.set_pid(job.id, 0);
}

async fn cut(cfg: &AppConfig, db: Arc<JobsDb>, hub: &Arc<ProcessHub>, job: &mut Job) {
    let root = match cfg.finished_path() {
        Ok(p) => p,
        Err(e) => {
            job.status = JobStatus::Failed;
            job.log = e.to_string();
            return;
        }
    };
    let src_name = job.urls.first().cloned().unwrap_or_default();
    let Some(src) = resolve_finished_file(&root, &src_name) else {
        job.status = JobStatus::Failed;
        job.log = "Invalid source file path".into();
        return;
    };
    if !src.is_file() {
        job.status = JobStatus::Failed;
        job.log = format!("Source file not found: {src_name}");
        return;
    }
    let extra = job.extra_object();
    let start = extra.get("start").and_then(|v| v.as_str()).unwrap_or("0");
    let end = extra.get("end").and_then(|v| v.as_str());
    let mode = extra.get("mode").and_then(|v| v.as_str()).unwrap_or("fast");
    let output = extra.get("output").and_then(|v| v.as_str()).unwrap_or("cut.bin");
    let dst = src.parent().unwrap_or(&root).join(output);
    let ffmpeg = ffmpeg_bin();
    let cmd = ffmpeg::cut_command(&ffmpeg.display().to_string(), &src, &dst, start, end, mode);
    job.log = format!("[cmd] {}\n", ffmpeg::display(&cmd));
    let _ = db.set_log(job.id, &job.log);
    let args: Vec<String> = cmd.iter().skip(1).cloned().collect();
    let prefix = job.log.clone();
    let job_id = job.id;
    let db_log = Arc::clone(&db);
    match hub
        .run(job_id, &cmd[0], &args, move |cleaned| {
            let _ = db_log.set_log(job_id, &format!("{prefix}{cleaned}"));
        })
        .await
    {
        Ok(0) => {
            job.status = JobStatus::Completed;
            if let Ok(Some(row)) = db.get_job(job.id) {
                job.log = row.log;
            }
        }
        Ok(_) | Err(_) => {
            job.status = JobStatus::Failed;
            if let Ok(Some(row)) = db.get_job(job.id) {
                job.log = row.log;
            }
            if dst.is_file() {
                let _ = std::fs::remove_file(&dst);
            }
        }
    }
}

pub async fn run_update_job(
    cfg: &AppConfig,
    db: &Arc<JobsDb>,
    hub: &Arc<ProcessHub>,
    job: &mut Job,
) {
    update(cfg, Arc::clone(db), hub, job).await;
}

async fn update(_cfg: &AppConfig, db: Arc<JobsDb>, hub: &Arc<ProcessHub>, job: &mut Job) {
    let bin = ytdlp_bin();
    let channel = job
        .extra_params
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or("nightly");
    let old = ytdlp::version(&bin).await;
    let args = vec!["--update-to".to_string(), channel.to_string()];
    let mut log = format!("[cmd] {} --update-to {channel}\nCurrent version: {old}\n", bin.display());
    job.log = log.clone();
    let _ = db.set_log(job.id, &job.log);
    let job_id = job.id;
    let prefix = log.clone();
    let db_log = Arc::clone(&db);
    match hub
        .run(job_id, &bin.display().to_string(), &args, move |cleaned| {
            let _ = db_log.set_log(job_id, &format!("{prefix}{cleaned}"));
        })
        .await
    {
        Ok(0) => {
            let new = ytdlp::version(&bin).await;
            if let Ok(Some(row)) = db.get_job(job.id) {
                log = row.log;
            }
            let msg = if new == old {
                format!("Already up to date ({new})")
            } else {
                format!("Updated {old} → {new}")
            };
            job.log = format!("{log}\n{msg}\n");
            job.status = JobStatus::Completed;
            let mut extra = job.extra_object();
            extra.insert("old_version".into(), json!(old));
            extra.insert("new_version".into(), json!(new));
            extra.insert("message".into(), json!(msg));
            extra.insert("success".into(), json!(true));
            job.extra_params = Value::Object(extra);
        }
        Ok(rc) => {
            if let Ok(Some(row)) = db.get_job(job.id) {
                log = row.log;
            }
            job.log = format!("{log}\nUpdate failed (exit {rc})\n");
            job.status = JobStatus::Failed;
            let mut extra = job.extra_object();
            extra.insert("success".into(), json!(false));
            extra.insert("message".into(), json!(format!("Update failed (exit {rc})")));
            job.extra_params = Value::Object(extra);
        }
        Err(e) => {
            job.log = format!("{log}Error: {e}\n");
            job.status = JobStatus::Failed;
        }
    }
}

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub async fn resume_pending(db: &JobsDb, pool: &WorkerPool) {
    if let Ok(jobs) = db.pending_or_running() {
        for row in jobs {
            let job = row.into_job();
            let _ = db.update_job(&job);
            pool.enqueue(job);
        }
    }
}
