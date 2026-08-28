use crate::jobs::format_cmd;
use std::path::Path;

pub fn cut_command(
    ffmpeg: &str,
    src: &Path,
    dst: &Path,
    start: &str,
    end: Option<&str>,
    mode: &str,
) -> Vec<String> {
    let mut cmd = vec![
        ffmpeg.to_string(),
        "-nostdin".into(),
        "-hide_banner".into(),
        "-y".into(),
        "-ss".into(),
        start.to_string(),
    ];
    if let Some(end) = end {
        cmd.push("-to".into());
        cmd.push(end.to_string());
    }
    cmd.push("-i".into());
    cmd.push(src.display().to_string());
    if mode != "precise" {
        cmd.extend([
            "-c".into(),
            "copy".into(),
            "-avoid_negative_ts".into(),
            "make_zero".into(),
        ]);
    }
    cmd.push(dst.display().to_string());
    cmd
}

pub fn display(cmd: &[String]) -> String {
    format_cmd(cmd)
}
