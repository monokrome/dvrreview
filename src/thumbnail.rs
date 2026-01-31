use anyhow::{Context, Result, bail};
use std::path::Path;
use tokio::process::Command;

pub async fn extract_thumbnail(video_path: &Path, timestamp_ms: i32) -> Result<Vec<u8>> {
    let ts_secs = timestamp_ms as f64 / 1000.0;

    let tmp = tempfile::Builder::new()
        .suffix(".jpg")
        .tempfile()
        .context("Failed to create temp file for thumbnail")?;

    let tmp_path = tmp.path().to_path_buf();

    let output = Command::new("ffmpeg")
        .args([
            "-ss",
            &format!("{:.3}", ts_secs),
            "-i",
        ])
        .arg(video_path)
        .args([
            "-vframes",
            "1",
            "-vf",
            "scale=320:-1",
            "-y",
        ])
        .arg(&tmp_path)
        .output()
        .await
        .context("Failed to run ffmpeg")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ffmpeg failed: {}", stderr);
    }

    let data = tokio::fs::read(&tmp_path)
        .await
        .context("Failed to read thumbnail output")?;

    if data.is_empty() {
        bail!("ffmpeg produced empty thumbnail");
    }

    Ok(data)
}
