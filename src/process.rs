use crate::jobs::clean_logs;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, Mutex};
use tokio::time::{interval, Duration};

#[derive(Clone, Debug)]
pub struct ProcEvent {
    pub log: String,
    pub percent: Option<f64>,
}

pub struct ProcessHub {
    children: Mutex<HashMap<i64, i32>>,
    events: Mutex<HashMap<i64, broadcast::Sender<ProcEvent>>>,
}

impl ProcessHub {
    pub fn new() -> Self {
        Self {
            children: Mutex::new(HashMap::new()),
            events: Mutex::new(HashMap::new()),
        }
    }

    pub async fn subscribe(&self, job_id: i64) -> broadcast::Receiver<ProcEvent> {
        let mut map = self.events.lock().await;
        map.entry(job_id)
            .or_insert_with(|| broadcast::channel(64).0)
            .subscribe()
    }

    async fn sender(&self, job_id: i64) -> broadcast::Sender<ProcEvent> {
        let mut map = self.events.lock().await;
        map.entry(job_id)
            .or_insert_with(|| broadcast::channel(64).0)
            .clone()
    }

    pub async fn publish(&self, job_id: i64, event: ProcEvent) {
        let tx = self.sender(job_id).await;
        let _ = tx.send(event);
    }

    pub async fn pid_of(&self, job_id: i64) -> Option<i32> {
        self.children.lock().await.get(&job_id).copied()
    }

    pub async fn interrupt(&self, pid: i32) {
        let _ = Command::new("kill")
            .args(["-INT", &pid.to_string()])
            .status()
            .await;
    }

    pub async fn run(
        self: &Arc<Self>,
        job_id: i64,
        program: &str,
        args: &[String],
        on_log: impl Fn(String) + Send + Sync + 'static,
    ) -> std::io::Result<i32> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child: Child = cmd.spawn()?;
        let pid = child.id().unwrap_or(0) as i32;
        self.children.lock().await.insert(job_id, pid);

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let log = Arc::new(Mutex::new(String::new()));
        let hub = Arc::clone(self);
        let log_cb = Arc::new(on_log);

        let pipe = |stream: Option<tokio::process::ChildStdout>,
                    err_stream: Option<tokio::process::ChildStderr>,
                    log: Arc<Mutex<String>>,
                    hub: Arc<ProcessHub>,
                    job_id: i64,
                    cb: Arc<dyn Fn(String) + Send + Sync>| async move {
            let mut combined = String::new();
            if let Some(out) = stream {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    combined.push_str(&line);
                    combined.push('\n');
                    let cleaned = clean_logs(&combined);
                    *log.lock().await = cleaned.clone();
                    let percent = parse_percent(&line);
                    hub.publish(
                        job_id,
                        ProcEvent {
                            log: cleaned.clone(),
                            percent,
                        },
                    )
                    .await;
                    cb(cleaned);
                }
            }
            if let Some(err) = err_stream {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    combined.push_str(&line);
                    combined.push('\n');
                    let cleaned = clean_logs(&combined);
                    *log.lock().await = cleaned.clone();
                    let percent = parse_percent(&line);
                    hub.publish(
                        job_id,
                        ProcEvent {
                            log: cleaned.clone(),
                            percent,
                        },
                    )
                    .await;
                    cb(cleaned);
                }
            }
        };

        // Split stdout/stderr piping concurrently. The helper above takes both;
        // call two simpler tasks instead.
        let log_out = Arc::clone(&log);
        let hub_out = Arc::clone(&hub);
        let cb_out = Arc::clone(&log_cb);
        let t_out = tokio::spawn(async move {
            if let Some(out) = stdout {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut g = log_out.lock().await;
                    g.push_str(&line);
                    g.push('\n');
                    let cleaned = clean_logs(&g);
                    drop(g);
                    let percent = parse_percent(&line);
                    hub_out
                        .publish(
                            job_id,
                            ProcEvent {
                                log: cleaned.clone(),
                                percent,
                            },
                        )
                        .await;
                    cb_out(cleaned);
                }
            }
        });

        let log_err = Arc::clone(&log);
        let hub_err = Arc::clone(&hub);
        let cb_err = Arc::clone(&log_cb);
        let t_err = tokio::spawn(async move {
            if let Some(err) = stderr {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut g = log_err.lock().await;
                    g.push_str(&line);
                    g.push('\n');
                    let cleaned = clean_logs(&g);
                    drop(g);
                    let percent = parse_percent(&line);
                    hub_err
                        .publish(
                            job_id,
                            ProcEvent {
                                log: cleaned.clone(),
                                percent,
                            },
                        )
                        .await;
                    cb_err(cleaned);
                }
            }
        });

        let status = child.wait().await?;
        let _ = t_out.await;
        let _ = t_err.await;
        self.children.lock().await.remove(&job_id);
        let _ = pipe;
        let _ = interval(Duration::from_secs(3));
        Ok(status.code().unwrap_or(1))
    }
}

pub fn parse_percent(line: &str) -> Option<f64> {
    let re = regex::Regex::new(r"(?i)(?:\[download\]\s+)?(\d{1,3}(?:\.\d+)?)%").ok()?;
    re.captures(line)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
        .map(|p: f64| p.clamp(0.0, 100.0))
}
