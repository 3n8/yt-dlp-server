use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const YDL_FORMATS: &[(&str, &[(&str, &str)])] = &[
    (
        "Video",
        &[
            ("video/best", "Best"),
            ("video/bestvideo", "Best Video"),
            ("video/mp4", "MP4"),
            ("video/flv", "Flash Video (FLV)"),
            ("video/webm", "WebM"),
            ("video/ogg", "Ogg"),
            ("video/mkv", "Matroska (MKV)"),
            ("video/avi", "AVI"),
        ],
    ),
    (
        "Audio",
        &[
            ("bestaudio/best", "Best Audio"),
            ("audio/aac", "AAC"),
            ("audio/flac", "FLAC"),
            ("audio/mp3", "MP3"),
            ("audio/m4a", "M4A"),
            ("audio/opus", "Opus"),
            ("audio/vorbis", "Vorbis"),
            ("audio/wav", "WAV"),
        ],
    ),
];

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppConfig {
    pub ydl_server: YdlServerConfig,
    #[serde(default)]
    pub ydl_options: BTreeMap<String, Value>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    #[serde(default)]
    pub aliases: BTreeMap<String, Alias>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct YdlServerConfig {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default)]
    pub debug: bool,
    #[serde(default = "default_db_path")]
    pub metadata_db_path: String,
    #[serde(default = "default_output_playlist")]
    pub output_playlist: String,
    #[serde(default = "default_max_logs")]
    pub max_log_entries: usize,
    #[serde(default = "default_format")]
    pub default_format: String,
    #[serde(default = "default_workers")]
    pub download_workers_count: usize,
    #[serde(default = "default_true")]
    pub schedule_upcoming: bool,
    #[serde(default = "default_schedule_interval")]
    pub schedule_check_interval: u64,
    #[serde(default = "default_schedule_attempts")]
    pub schedule_max_attempts: u32,
    #[serde(default = "default_update_channel")]
    pub update_channel: String,
}

fn default_port() -> u16 {
    8080
}
fn default_host() -> String {
    "0.0.0.0".into()
}
fn default_db_path() -> String {
    "/config/jobs.db".into()
}
fn default_output_playlist() -> String {
    format!(
        "{}/{}",
        download_root(),
        "%(playlist_title)s [%(playlist_id)s]/%(title)s.%(ext)s"
    )
}

pub fn download_root() -> String {
    std::env::var("DOWNLOADS")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/downloads".into())
}

fn rebase_download_path(path: &str) -> String {
    let root = download_root();
    for prefix in ["/downloads", "/data"] {
        if path == prefix {
            return root.clone();
        }
        if let Some(rest) = path.strip_prefix(prefix) {
            if rest.starts_with('/') {
                return format!("{root}{rest}");
            }
        }
    }
    path.to_string()
}

fn rebase_value(v: &mut Value) {
    if let Value::String(s) = v {
        *s = rebase_download_path(s);
    }
}

fn apply_download_root(cfg: &mut AppConfig) {
    if let Some(v) = cfg.ydl_options.get_mut("output") {
        rebase_value(v);
    }
    if let Some(v) = cfg.ydl_options.get_mut("paths") {
        rebase_value(v);
    }
    cfg.ydl_server.output_playlist = rebase_download_path(&cfg.ydl_server.output_playlist);
    for profile in cfg.profiles.values_mut() {
        if let Some(v) = profile.ydl_options.get_mut("output") {
            rebase_value(v);
        }
        if let Some(v) = profile.ydl_options.get_mut("paths") {
            rebase_value(v);
        }
    }
}
fn default_max_logs() -> usize {
    100
}
fn default_format() -> String {
    "video/best".into()
}
fn default_workers() -> usize {
    2
}
fn default_true() -> bool {
    true
}
fn default_schedule_interval() -> u64 {
    60
}
fn default_schedule_attempts() -> u32 {
    24
}
fn default_update_channel() -> String {
    "nightly".into()
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Profile {
    pub name: Option<String>,
    #[serde(default)]
    pub ydl_options: BTreeMap<String, Value>,
    #[serde(default)]
    pub use_: Option<Value>,
    #[serde(rename = "use", default)]
    pub use_list: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Alias {
    pub name: Option<String>,
    #[serde(default)]
    pub ydl_options: BTreeMap<String, Value>,
    #[serde(default, rename = "use")]
    pub use_list: Option<Value>,
    #[serde(default = "default_true")]
    pub ui: bool,
}

impl AppConfig {
    pub fn load() -> Result<Self, ConfigError> {
        let path = config_file_path();
        tracing::info!("Using configuration file {}", path.display());
        if !path.exists() {
            let default = default_config_path();
            if default.exists() {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                if let Err(e) = fs::copy(&default, &path) {
                    tracing::warn!("Could not copy default config to {}: {e}", path.display());
                    return load_from(&default);
                }
            } else {
                return Err(ConfigError::Message(format!(
                    "{} does not exist and no default_config.yml found",
                    path.display()
                )));
            }
        }
        let mut cfg = load_from(&path)?;
        resolve_aliases(&mut cfg)?;
        apply_download_root(&mut cfg);
        tracing::info!("Download directory {}", download_root());
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let output = self
            .ydl_options
            .get("output")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ConfigError::Message("ydl_options.output is required".into()))?;
        let finished = finished_path_from(output, self.ydl_options.get("paths"))?;
        fs::create_dir_all(&finished)?;
        Ok(())
    }

    pub fn ydl_formats(&self) -> BTreeMap<String, BTreeMap<String, String>> {
        let mut out = BTreeMap::new();
        for (cat, items) in YDL_FORMATS {
            let mut map = BTreeMap::new();
            for (k, v) in *items {
                map.insert((*k).to_string(), (*v).to_string());
            }
            out.insert((*cat).to_string(), map);
        }
        if !self.profiles.is_empty() {
            let mut map = BTreeMap::new();
            for (k, p) in &self.profiles {
                map.insert(
                    format!("profile/{k}"),
                    p.name.clone().unwrap_or_else(|| k.clone()),
                );
            }
            out.insert("Profiles".into(), map);
        }
        out
    }

    pub fn ui_aliases(&self) -> BTreeMap<String, String> {
        self.aliases
            .iter()
            .filter(|(_, a)| a.ui)
            .map(|(k, a)| (k.clone(), a.name.clone().unwrap_or_else(|| k.clone())))
            .collect()
    }

    pub fn finished_path(&self) -> Result<PathBuf, ConfigError> {
        let output = self
            .ydl_options
            .get("output")
            .and_then(|v| v.as_str())
            .unwrap_or("/downloads/%(title)s [%(id)s].%(ext)s");
        finished_path_from(output, self.ydl_options.get("paths"))
    }
}

fn load_from(path: &Path) -> Result<AppConfig, ConfigError> {
    let text = fs::read_to_string(path)?;
    let cfg: AppConfig = serde_yaml::from_str(&text)?;
    Ok(cfg)
}

pub fn config_file_path() -> PathBuf {
    let raw = std::env::var("YDL_CONFIG_PATH").unwrap_or_else(|_| "/config".into());
    let p = PathBuf::from(&raw);
    if p.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.contains('.'))
        .unwrap_or(false)
    {
        p
    } else {
        p.join("config.yml")
    }
}

fn default_config_path() -> PathBuf {
    PathBuf::from(
        std::env::var("YDL_DEFAULT_CONFIG").unwrap_or_else(|_| "/usr/lib/yt-dlp-server/default_config.yml".into()),
    )
}

fn normalize_use(use_val: Option<&Value>) -> Vec<String> {
    match use_val {
        None => vec![],
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(seq)) => seq
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => vec![],
    }
}

fn expand_alias(
    name: &str,
    aliases: &BTreeMap<String, Alias>,
    stack: &mut Vec<String>,
) -> Result<BTreeMap<String, Value>, ConfigError> {
    if stack.iter().any(|s| s == name) {
        stack.push(name.into());
        return Err(ConfigError::Message(format!(
            "Recursive alias definition: {}",
            stack.join(" -> ")
        )));
    }
    let alias = aliases
        .get(name)
        .ok_or_else(|| ConfigError::Message(format!("Unknown alias '{name}'")))?;
    stack.push(name.into());
    let mut options = expand_uses(alias.use_list.as_ref(), aliases, stack)?;
    options.extend(alias.ydl_options.clone());
    stack.pop();
    Ok(options)
}

fn expand_uses(
    use_val: Option<&Value>,
    aliases: &BTreeMap<String, Alias>,
    stack: &mut Vec<String>,
) -> Result<BTreeMap<String, Value>, ConfigError> {
    let mut options = BTreeMap::new();
    for name in normalize_use(use_val) {
        options.extend(expand_alias(&name, aliases, stack)?);
    }
    Ok(options)
}

fn resolve_aliases(cfg: &mut AppConfig) -> Result<(), ConfigError> {
    let alias_snapshot = cfg.aliases.clone();
    for (name, alias) in cfg.aliases.iter_mut() {
        let mut stack = Vec::new();
        alias.ydl_options = expand_alias(name, &alias_snapshot, &mut stack)?;
        alias.use_list = None;
    }
    let aliases = cfg.aliases.clone();
    for profile in cfg.profiles.values_mut() {
        let use_val = profile.use_list.as_ref().or(profile.use_.as_ref());
        let mut options = expand_uses(use_val, &aliases, &mut Vec::new())?;
        options.extend(profile.ydl_options.clone());
        profile.ydl_options = options;
        profile.use_list = None;
        profile.use_ = None;
    }
    Ok(())
}

fn get_static_prefix(output_template: &str) -> String {
    let mut prefix = Vec::new();
    for s in output_template.split('/') {
        if s.replace("%%", "").contains('%') {
            break;
        }
        prefix.push(s);
    }
    if prefix == [""] {
        return "/".into();
    }
    prefix.join("/")
}

fn paths_home(paths: Option<&Value>) -> Option<String> {
    let paths = paths?;
    let s = match paths {
        Value::String(s) => s.clone(),
        other => other.as_str()?.to_string(),
    };
    let (path_type, rest) = s.split_once(':')?;
    if rest.is_empty() {
        return Some(s);
    }
    if path_type == "home" {
        return Some(rest.to_string());
    }
    None
}

fn finished_path_from(output: &str, paths: Option<&Value>) -> Result<PathBuf, ConfigError> {
    let mut prefix = get_static_prefix(output);
    if !Path::new(&prefix).is_absolute() {
        let home = paths_home(paths).unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .display()
                .to_string()
        });
        prefix = format!("{}/{}", home.trim_end_matches('/'), prefix.trim_start_matches('/'));
    }
    let finished = PathBuf::from(&prefix);
    let finished = if finished.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        finished
    };
    if finished == Path::new("/") {
        return Err(ConfigError::Message(format!(
            "Could not determine the download directory from ydl_options.output ('{output}'): it resolves to the filesystem root."
        )));
    }
    Ok(finished)
}

pub fn resolve_finished_file(root: &Path, fname: &str) -> Option<PathBuf> {
    if fname.is_empty() {
        return None;
    }
    let root = fs::canonicalize(root).ok()?;
    let joined = root.join(fname);
    let path = fs::canonicalize(&joined).unwrap_or(joined);
    if path == root {
        return Some(path);
    }
    path.starts_with(&root).then_some(path)
}

pub fn ytdlp_bin() -> PathBuf {
    PathBuf::from(std::env::var("YTDLP_BIN").unwrap_or_else(|_| "/config/bin/yt-dlp".into()))
}

pub fn ffmpeg_bin() -> PathBuf {
    PathBuf::from(std::env::var("FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".into()))
}

pub fn ydls_version() -> String {
    std::env::var("YDLS_VERSION").unwrap_or_default()
}

pub fn ydls_release_date() -> String {
    std::env::var("YDLS_RELEASE_DATE").unwrap_or_default()
}
