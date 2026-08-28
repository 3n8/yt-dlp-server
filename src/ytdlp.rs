use crate::config::AppConfig;
use crate::jobs::format_cmd;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

pub fn prefix_format(prefix: &str, value: &str) -> String {
    let start = format!("{prefix}/");
    if value.starts_with(&start) {
        value.to_string()
    } else {
        format!("{prefix}/{value}")
    }
}

pub fn value_to_opt(v: &Value) -> Option<String> {
    match v {
        Value::Bool(false) => None,
        Value::Bool(true) => Some(String::new()),
        Value::Null => Some(String::new()),
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

pub fn opts_to_args(opts: &BTreeMap<String, Value>) -> Vec<String> {
    let mut args = Vec::new();
    for (key, val) in opts {
        if matches!(val, Value::Bool(false)) {
            continue;
        }
        args.push(format!("--{key}"));
        if let Some(s) = value_to_opt(val) {
            if !s.is_empty() {
                args.push(s);
            }
        }
    }
    args
}

fn split_format(format_string: &str) -> (Option<String>, Option<String>, Option<String>, Vec<String>) {
    let mut fmt = None;
    let mut audio = None;
    let mut profile = None;
    let mut aliases = Vec::new();
    for s in format_string.split(',').filter(|s| !s.is_empty()) {
        if s.starts_with("profile/") {
            profile = Some(s.to_string());
        } else if s.starts_with("alias/") {
            aliases.push(s.to_string());
        } else if s.starts_with("audio/") || s.starts_with("bestaudio/") {
            audio = Some(s.to_string());
        } else {
            fmt = Some(s.to_string());
        }
    }
    (fmt, audio, profile, aliases)
}

pub fn merge_ydl_options(cfg: &AppConfig, format_string: Option<&str>) -> Result<BTreeMap<String, Value>, String> {
    let mut ydl = cfg.ydl_options.clone();
    let (mut req_format, req_audio, req_profile, req_aliases) =
        split_format(format_string.unwrap_or(""));

    let mut profile_opts = BTreeMap::new();
    if let Some(p) = &req_profile {
        let name = p.splitn(2, '/').nth(1).unwrap_or(p);
        let profile = cfg
            .profiles
            .get(name)
            .ok_or_else(|| format!("Unknown profile {p}"))?;
        profile_opts = profile.ydl_options.clone();
        if req_format.is_none() {
            req_format = profile_opts
                .get("format")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }
    }

    let mut alias_opts = BTreeMap::new();
    for a in &req_aliases {
        let name = a.splitn(2, '/').nth(1).unwrap_or(a);
        let alias = cfg
            .aliases
            .get(name)
            .ok_or_else(|| format!("Unknown alias {a}"))?;
        alias_opts.extend(alias.ydl_options.clone());
        if req_format.is_none() {
            req_format = alias_opts
                .get("format")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }
    }

    if req_audio.is_some() && req_format.is_none() {
        ydl.insert("extract-audio".into(), Value::Null);
        if let Some(a) = &req_audio {
            ydl.insert(
                "audio-format".into(),
                Value::String(a.rsplit('/').next().unwrap_or(a).to_string()),
            );
        }
    }

    if let Some(mut fmt) = req_format {
        if fmt == "video/best" {
            fmt = "video/bestvideo".into();
        }
        if fmt.starts_with("video/") && fmt != "video/best" {
            fmt = fmt.rsplit('/').next().unwrap_or(&fmt).to_string();
        }
        if let Some(a) = &req_audio {
            fmt = format!("{fmt}+{}", a.rsplit('/').next().unwrap_or(a));
        } else {
            fmt = format!("{fmt}+bestaudio/best");
        }
        ydl.insert("format".into(), Value::String(fmt));
    } else if req_audio.is_none() {
        ydl.insert("format".into(), Value::String("video/best".into()));
    }

    profile_opts.remove("format");
    alias_opts.remove("format");
    ydl.extend(profile_opts);
    ydl.extend(alias_opts);
    Ok(ydl)
}

pub fn build_cmd(
    bin: &PathBuf,
    opts: &BTreeMap<String, Value>,
    urls: &[String],
    extra: &[&str],
) -> Vec<String> {
    let mut cmd = vec![bin.display().to_string()];
    cmd.extend(opts_to_args(opts));
    for e in extra {
        cmd.push((*e).to_string());
    }
    cmd.push("--".into());
    cmd.extend(urls.iter().cloned());
    cmd
}

pub fn cmd_display(cmd: &[String]) -> String {
    format_cmd(cmd)
}

pub async fn run_capture(bin: &PathBuf, args: &[String]) -> std::io::Result<(i32, String, String)> {
    let out = Command::new(bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;
    Ok((
        out.status.code().unwrap_or(1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

pub async fn version(bin: &PathBuf) -> String {
    match run_capture(bin, &["--version".into()]).await {
        Ok((0, stdout, _)) => stdout.trim().to_string(),
        Ok((_, stdout, stderr)) => stdout.trim().to_string() + stderr.trim(),
        Err(e) => format!("unknown ({e})"),
    }
}

pub async fn list_extractors(bin: &PathBuf) -> Vec<String> {
    match run_capture(bin, &["--list-extractors".into()]).await {
        Ok((0, stdout, _)) => stdout
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => vec![],
    }
}

pub fn parse_json_lines(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}
