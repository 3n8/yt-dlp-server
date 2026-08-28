use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum JobStatus {
    Running = 0,
    Completed = 1,
    Failed = 2,
    Pending = 3,
    Aborted = 4,
    Scheduled = 5,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Pending => "Pending",
            Self::Aborted => "Aborted",
            Self::Scheduled => "Scheduled",
        }
    }

    pub fn from_i32(v: i32) -> Option<Self> {
        Some(match v {
            0 => Self::Running,
            1 => Self::Completed,
            2 => Self::Failed,
            3 => Self::Pending,
            4 => Self::Aborted,
            5 => Self::Scheduled,
            _ => return None,
        })
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "pending" => Some(Self::Pending),
            "aborted" => Some(Self::Aborted),
            "scheduled" => Some(Self::Scheduled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum JobType {
    YdlDownload = 0,
    YtdlpUpdate = 1,
    FfmpegCut = 2,
}

impl JobType {
    pub fn from_i32(v: i32) -> Option<Self> {
        Some(match v {
            0 => Self::YdlDownload,
            1 => Self::YtdlpUpdate,
            2 => Self::FfmpegCut,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: i64,
    pub name: String,
    pub status: JobStatus,
    pub log: String,
    pub format: Option<String>,
    pub job_type: JobType,
    pub urls: Vec<String>,
    pub pid: i32,
    pub force_generic_extractor: bool,
    pub extra_params: Value,
    pub scheduled_at: Option<i64>,
}

impl Job {
    pub fn new(name: String, job_type: JobType, format: Option<String>, urls: Vec<String>) -> Self {
        Self {
            id: -1,
            name,
            status: JobStatus::Pending,
            log: String::new(),
            format,
            job_type,
            urls,
            pid: 0,
            force_generic_extractor: false,
            extra_params: Value::Object(Default::default()),
            scheduled_at: None,
        }
    }

    pub fn extra_object(&self) -> serde_json::Map<String, Value> {
        self.extra_params
            .as_object()
            .cloned()
            .unwrap_or_default()
    }
}

pub fn clean_logs(logs: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r".*\r").unwrap());
    logs.lines()
        .map(|line| re.replace(line, "").into_owned())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn format_cmd(cmd: &[String]) -> String {
    const SENSITIVE: &[&str] = &[
        "--username",
        "--password",
        "--video-password",
        "--ap-username",
        "--ap-password",
        "--client-secret",
        "--add-header",
    ];
    let mut out = Vec::new();
    let mut redact = false;
    for arg in cmd {
        if redact {
            out.push("***".into());
        } else {
            out.push(shell_quote(arg));
        }
        redact = SENSITIVE.contains(&arg.as_str());
    }
    out.join(" ")
}

fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./:@+".contains(c))
    {
        return s.into();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}
