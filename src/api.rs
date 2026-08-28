use crate::config::{ffmpeg_bin, resolve_finished_file, ytdlp_bin};
use crate::files::build_tree;
use crate::jobs::{Job, JobStatus, JobType};
use crate::process::parse_percent;
use crate::state::AppState;
use crate::ytdlp;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tower_http::services::{ServeDir, ServeFile};

pub fn router(state: AppState, static_dir: PathBuf) -> axum::Router {
    let index = static_dir.join("index.html");
    let static_service = ServeDir::new(&static_dir)
        .append_index_html_on_directories(true)
        .fallback(ServeFile::new(index));

    axum::Router::new()
        .route("/api/extractors", axum::routing::get(api_extractors))
        .route("/api/formats", axum::routing::get(api_formats))
        .route("/api/info", axum::routing::get(api_info))
        .route("/api/downloads/stats", axum::routing::get(api_stats))
        .route("/api/downloads/clean", axum::routing::post(api_clean))
        .route(
            "/api/downloads",
            axum::routing::get(api_logs)
                .post(api_queue)
                .delete(api_purge),
        )
        .route("/api/metadata", axum::routing::post(api_metadata))
        .route("/api/finished", axum::routing::get(api_finished))
        .route(
            "/api/finished/{fname}/cut",
            axum::routing::post(api_cut_file),
        )
        .route(
            "/api/finished/{fname}",
            axum::routing::get(api_get_file).delete(api_delete_file),
        )
        .route("/api/jobs/{job_id}/stop", axum::routing::post(api_stop))
        .route("/api/jobs/{job_id}/retry", axum::routing::post(api_retry))
        .route("/api/jobs/{job_id}/events", axum::routing::get(api_events))
        .route("/api/jobs/{job_id}", axum::routing::delete(api_delete_job))
        .route("/api/yt-dlp/update", axum::routing::post(api_update))
        .fallback_service(static_service)
        .with_state(state)
}

async fn api_extractors(State(st): State<AppState>) -> impl IntoResponse {
    Json(st.extractors.read().await.clone())
}

async fn api_formats(State(st): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "ydl_formats": st.config.ydl_formats(),
        "ydl_aliases": st.config.ui_aliases(),
        "ydl_default_format": st.config.ydl_server.default_format,
    }))
}

async fn api_info(State(st): State<AppState>) -> impl IntoResponse {
    let version = st.ydl_version.read().await.clone();
    Json(json!({
        "ydl_module_name": "yt-dlp",
        "ydl_module_version": version,
        "ydl_module_website": "https://github.com/yt-dlp/yt-dlp",
        "ydls_version": crate::config::ydls_version(),
        "ydls_release_date": crate::config::ydls_release_date(),
        "download_workers_count": st.config.ydl_server.download_workers_count,
        "update_channel": st.config.ydl_server.update_channel,
        "update_in_progress": st.pool.update_lock.load(Ordering::SeqCst),
    }))
}

async fn api_stats(State(st): State<AppState>) -> impl IntoResponse {
    let mut stats = st.db.job_counts().unwrap_or_default();
    stats.insert("queue".into(), stats.get("pending").copied().unwrap_or(0));
    Json(json!({ "success": true, "stats": stats }))
}

async fn api_logs(
    State(st): State<AppState>,
    Query(q): Query<LogsQuery>,
) -> impl IntoResponse {
    let with_logs = q
        .show_logs
        .as_deref()
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(true);
    let limit = st.config.ydl_server.max_log_entries;
    let jobs = st
        .db
        .get_jobs(limit, q.status.as_deref(), with_logs)
        .unwrap_or_default();
    Json(jobs.iter().map(|j| j.to_json()).collect::<Vec<_>>())
}

#[derive(Deserialize)]
struct LogsQuery {
    status: Option<String>,
    show_logs: Option<String>,
}

async fn api_purge(State(st): State<AppState>) -> impl IntoResponse {
    let _ = st.db.purge();
    Json(json!({ "success": true }))
}

async fn api_clean(State(st): State<AppState>) -> impl IntoResponse {
    let _ = st.db.clean_old(st.config.ydl_server.max_log_entries);
    Json(json!({ "success": true }))
}

#[derive(Deserialize, Default)]
struct QueueBody {
    url: Option<String>,
    urls: Option<Vec<String>>,
    format: Option<String>,
    profile: Option<String>,
    aliases: Option<Value>,
    audio_format: Option<String>,
    force_generic_extractor: Option<Value>,
    extra_params: Option<Value>,
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "1" || s.eq_ignore_ascii_case("true"),
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        _ => false,
    }
}

fn parse_aliases(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => s.split(',').filter(|a| !a.is_empty()).map(|s| s.to_string()).collect(),
        Some(Value::Array(seq)) => seq
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect(),
        _ => vec![],
    }
}

fn prefix_format(prefix: &str, value: &str) -> String {
    ytdlp::prefix_format(prefix, value)
}

async fn api_queue(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let data: QueueBody = if headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .contains("application/x-www-form-urlencoded")
    {
        let parsed: Vec<(String, String)> =
            serde_urlencoded::from_bytes(&body).unwrap_or_default();
        let mut b = QueueBody::default();
        for (k, v) in parsed {
            match k.as_str() {
                "url" => b.url = Some(v),
                "format" => b.format = Some(v),
                _ => {}
            }
        }
        b
    } else {
        serde_json::from_slice(&body).unwrap_or_default()
    };

    let mut urls = data.urls.unwrap_or_default();
    if let Some(u) = data.url {
        if !u.is_empty() {
            urls.push(u);
        }
    }
    urls.retain(|u| !u.is_empty());
    if urls.is_empty() {
        return (
            StatusCode::OK,
            Json(json!({"success": false, "error": "'url' and 'urls' query parameters omitted"})),
        );
    }

    let mut format_str = data.format.clone();
    if let Some(p) = &data.profile {
        format_str = Some(
            [format_str.clone(), Some(prefix_format("profile", p))]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    let aliases = parse_aliases(data.aliases.as_ref());
    if !aliases.is_empty() {
        let extra = aliases
            .iter()
            .map(|a| prefix_format("alias", a))
            .collect::<Vec<_>>()
            .join(",");
        format_str = Some(
            [format_str.clone(), Some(extra)]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    if let Some(a) = &data.audio_format {
        format_str = Some(
            [format_str.clone(), Some(prefix_format("audio", a))]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    if format_str.as_deref().unwrap_or("").is_empty() {
        format_str = Some(st.config.ydl_server.default_format.clone());
    }
    let force = truthy(data.force_generic_extractor.as_ref());
    let extra = data.extra_params.unwrap_or(json!({}));

    let _ = st.db.clean_old(st.config.ydl_server.max_log_entries.saturating_sub(1));
    let mut job = Job::new(urls.join(", "), JobType::YdlDownload, format_str.clone(), urls.clone());
    job.force_generic_extractor = force;
    job.extra_params = extra;
    if let Err(e) = st.db.insert_job(&mut job) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"success": false, "error": e.to_string()})),
        );
    }
    let job_id = job.id;
    st.pool.enqueue(job);
    tracing::info!("Added url {} to the download queue", urls.join(","));
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "urls": urls,
            "options": { "format": format_str, "force_generic_extractor": force },
            "job_id": job_id,
        })),
    )
}

async fn api_metadata(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let data: QueueBody = if headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .contains("application/x-www-form-urlencoded")
    {
        QueueBody::default()
    } else {
        serde_json::from_slice(&body).unwrap_or_default()
    };
    let mut urls = data.urls.unwrap_or_default();
    if let Some(u) = data.url {
        urls.push(u);
    }
    urls.retain(|u| !u.is_empty());
    let force = truthy(data.force_generic_extractor.as_ref());
    let bin = ytdlp_bin();
    let mut extra = vec!["-J", "--flat-playlist"];
    if force {
        extra.push("--force-generic-extractor");
    }
    let cmd = ytdlp::build_cmd(&bin, &st.config.ydl_options, &urls, &extra);
    let args: Vec<String> = cmd.iter().skip(1).cloned().collect();
    match ytdlp::run_capture(&bin, &args).await {
        Ok((0, stdout, _)) => {
            let parsed = ytdlp::parse_json_lines(&stdout);
            Json(Value::Array(parsed)).into_response()
        }
        _ => (StatusCode::NOT_FOUND, Json(json!({"success": false}))).into_response(),
    }
}

async fn api_finished(State(st): State<AppState>) -> impl IntoResponse {
    match st.config.finished_path() {
        Ok(root) => Json(build_tree(&root)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn api_delete_file(State(st): State<AppState>, Path(fname): Path<String>) -> impl IntoResponse {
    if fname.is_empty() {
        return Json(json!({"success": false, "message": "No filename specified"}));
    }
    let Ok(root) = st.config.finished_path() else {
        return Json(json!({"success": false, "message": "Invalid filename"}));
    };
    let Some(path) = resolve_finished_file(&root, &fname) else {
        return Json(json!({"success": false, "message": "Invalid filename"}));
    };
    let res = if path.is_dir() {
        std::fs::remove_dir_all(&path)
    } else {
        std::fs::remove_file(&path)
    };
    match res {
        Ok(()) => Json(json!({"success": true, "message": "File deleted"})),
        Err(e) => Json(json!({
            "success": false,
            "message": format!("Could not delete the specified file (Err {})", e.raw_os_error().unwrap_or(-1))
        })),
    }
}

async fn api_get_file(State(st): State<AppState>, Path(fname): Path<String>) -> Response {
    let Ok(root) = st.config.finished_path() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(path) = resolve_finished_file(&root, &fname) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            Response::builder()
                .header(header::CONTENT_TYPE, mime.as_ref())
                .header(
                    header::CONTENT_DISPOSITION,
                    format!(
                        "inline; filename=\"{}\"",
                        path.file_name().and_then(|s| s.to_str()).unwrap_or("file")
                    ),
                )
                .body(Body::from(bytes))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Deserialize)]
struct CutBody {
    start: Option<String>,
    end: Option<Value>,
    mode: Option<String>,
    output: Option<String>,
}

async fn api_cut_file(
    State(st): State<AppState>,
    Path(fname): Path<String>,
    Json(body): Json<CutBody>,
) -> impl IntoResponse {
    let start = body.start.unwrap_or_else(|| "0".into());
    let end = match &body.end {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Null) | None => None,
        Some(other) => Some(other.to_string()),
    };
    let mode = body.mode.unwrap_or_else(|| "fast".into());
    let output = body.output.unwrap_or_default().trim().to_string();

    let Ok(root) = st.config.finished_path() else {
        return Json(json!({"success": false, "message": "Invalid filename"}));
    };
    let Some(src) = resolve_finished_file(&root, &fname) else {
        return Json(json!({"success": false, "message": "Invalid filename"}));
    };
    if !src.is_file() {
        return Json(json!({"success": false, "message": "File not found"}));
    }
    if output.is_empty() || output.contains('/') || output.starts_with('.') {
        return Json(json!({"success": false, "message": "Invalid output filename"}));
    }
    let dst = src.parent().unwrap_or(&root).join(&output);
    if dst.exists() {
        return Json(json!({"success": false, "message": "Output file already exists"}));
    }
    let ts_re = Regex::new(r"^(\d+(\.\d+)?|(\d+:)?[0-5]?\d:[0-5]?\d(\.\d+)?)$").unwrap();
    if !ts_re.is_match(&start) || end.as_ref().is_some_and(|e| !ts_re.is_match(e)) {
        return Json(json!({"success": false, "message": "Invalid timestamp"}));
    }
    if let Some(e) = &end {
        if parse_ts(e) <= parse_ts(&start) {
            return Json(json!({"success": false, "message": "End time must be after start time"}));
        }
    }
    if mode != "fast" && mode != "precise" {
        return Json(json!({"success": false, "message": "Invalid mode"}));
    }

    let mut job = Job::new(
        format!("Cut {fname} [{} - {}]", start, end.as_deref().unwrap_or("end")),
        JobType::FfmpegCut,
        None,
        vec![fname],
    );
    job.extra_params = json!({
        "start": start,
        "end": end,
        "mode": mode,
        "output": output,
    });
    let _ = st.db.clean_old(st.config.ydl_server.max_log_entries.saturating_sub(1));
    if st.db.insert_job(&mut job).is_err() {
        return Json(json!({"success": false, "message": "Could not queue cut"}));
    }
    st.pool.enqueue(job);
    Json(json!({"success": true, "output": output}))
}

fn parse_ts(ts: &str) -> f64 {
    ts.split(':')
        .filter_map(|p| p.parse::<f64>().ok())
        .fold(0.0, |acc, p| acc * 60.0 + p)
}

async fn api_stop(State(st): State<AppState>, Path(job_id): Path<String>) -> impl IntoResponse {
    let Ok(id) = job_id.parse::<i64>() else {
        return (StatusCode::NOT_FOUND, Json(json!({"success": false})));
    };
    let Some(job) = st.db.get_job(id).ok().flatten() else {
        return (StatusCode::NOT_FOUND, Json(json!({"success": false})));
    };
    match job.status {
        JobStatus::Pending | JobStatus::Scheduled => {
            let _ = st.db.set_status(id, JobStatus::Aborted);
            (StatusCode::OK, Json(json!({"success": true})))
        }
        JobStatus::Running if job.pid != 0 => {
            st.hub.interrupt(job.pid).await;
            (StatusCode::OK, Json(json!({"success": true})))
        }
        _ if job.pid == 0 => {
            let _ = st.db.set_status(id, JobStatus::Aborted);
            (StatusCode::OK, Json(json!({"success": true})))
        }
        _ => (StatusCode::OK, Json(json!({"success": false}))),
    }
}

async fn api_retry(State(st): State<AppState>, Path(job_id): Path<String>) -> impl IntoResponse {
    let Ok(id) = job_id.parse::<i64>() else {
        return (StatusCode::NOT_FOUND, Json(json!({"success": false})));
    };
    let Some(job) = st.db.get_job(id).ok().flatten() else {
        return (StatusCode::NOT_FOUND, Json(json!({"success": false})));
    };
    let mut extra = job.extra_params.clone();
    if let Some(obj) = extra.as_object_mut() {
        obj.remove("schedule_attempts");
    }
    let mut new_job = Job::new(job.name, job.job_type, job.format, job.urls);
    new_job.force_generic_extractor = job.force_generic_extractor;
    new_job.extra_params = extra;
    let _ = st.db.delete_job(id);
    let _ = st.db.insert_job(&mut new_job);
    st.pool.enqueue(new_job);
    (StatusCode::OK, Json(json!({"success": true})))
}

async fn api_delete_job(State(st): State<AppState>, Path(job_id): Path<String>) -> impl IntoResponse {
    if let Ok(id) = job_id.parse::<i64>() {
        let _ = st.db.delete_job(id);
        Json(json!({"success": true}))
    } else {
        Json(json!({"success": false}))
    }
}

async fn api_events(State(st): State<AppState>, Path(job_id): Path<String>) -> impl IntoResponse {
    let Ok(id) = job_id.parse::<i64>() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let db = Arc::clone(&st.db);
    let stream = async_stream::stream! {
        let mut last = String::new();
        loop {
            match db.get_job(id) {
                Ok(Some(job)) => {
                    if job.log != last {
                        last = job.log.clone();
                        let percent = last.lines().rev().find_map(parse_percent);
                        let data = json!({"log": last, "percent": percent});
                        yield Ok::<_, Infallible>(Event::default().event("log").data(data.to_string()));
                    }
                    if matches!(job.status, JobStatus::Completed | JobStatus::Failed | JobStatus::Aborted) {
                        let extra = job.extra_params;
                        let success = job.status == JobStatus::Completed;
                        let msg = extra
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or(if success { "Done" } else { "Failed" })
                            .to_string();
                        let data = json!({
                            "success": success,
                            "old_version": extra.get("old_version"),
                            "new_version": extra.get("new_version"),
                            "message": msg,
                            "log": last,
                        });
                        yield Ok(Event::default().event("done").data(data.to_string()));
                        break;
                    }
                }
                _ => break,
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    };
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

async fn api_update(State(st): State<AppState>) -> impl IntoResponse {
    if st
        .pool
        .update_lock
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
        || st.db.running_update().ok().flatten().is_some()
    {
        return Json(json!({"success": false, "error": "An update is already running"}));
    }
    let mut job = Job::new(
        format!("Update yt-dlp ({})", st.config.ydl_server.update_channel),
        JobType::YtdlpUpdate,
        None,
        vec![],
    );
    job.extra_params = json!({"channel": st.config.ydl_server.update_channel});
    if st.db.insert_job(&mut job).is_err() {
        st.pool.update_lock.store(false, Ordering::SeqCst);
        return Json(json!({"success": false, "error": "Could not queue update"}));
    }
    let job_id = job.id;
    let lock = Arc::clone(&st.pool.update_lock);
    let db = Arc::clone(&st.db);
    let hub = Arc::clone(&st.hub);
    let cfg = st.config.clone();
    let version_slot = Arc::clone(&st.ydl_version);
    tokio::spawn(async move {
        let mut job = job;
        crate::queue::run_update_job(&cfg, &db, &hub, &mut job).await;
        let _ = db.update_job(&job);
        if job.status == JobStatus::Completed {
            if let Some(v) = job.extra_params.get("new_version").and_then(|x| x.as_str()) {
                *version_slot.write().await = v.to_string();
            }
        }
        lock.store(false, Ordering::SeqCst);
    });
    Json(json!({"success": true, "job_id": job_id}))
}

// silence unused ffmpeg_bin import if cut uses queue
#[allow(dead_code)]
fn _ffmpeg() -> PathBuf {
    ffmpeg_bin()
}
