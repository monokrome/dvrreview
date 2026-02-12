use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::scanner::fingerprint::Fingerprinter;

#[derive(Debug)]
pub struct UnknownStream {
    pub index: usize,
    pub codec: String,
}

pub fn probe_unknown_streams(path: &Path) -> Result<Vec<UnknownStream>> {
    let output = Command::new("ffprobe")
        .args([
            "-v", "quiet",
            "-show_entries", "stream=index,codec_name,codec_type",
            "-of", "json",
        ])
        .arg(path)
        .output()
        .context("Failed to run ffprobe")?;

    let probe: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("Failed to parse ffprobe output")?;

    let streams = probe["streams"].as_array();
    let mut unknown = Vec::new();

    if let Some(streams) = streams {
        for stream in streams {
            let codec_type = stream["codec_type"].as_str().unwrap_or("");
            if codec_type == "unknown" || codec_type == "" {
                let index = stream["index"].as_u64().unwrap_or(0) as usize;
                let codec = stream["codec_name"].as_str().unwrap_or("none").to_string();
                unknown.push(UnknownStream { index, codec });
            }
        }
    }

    Ok(unknown)
}

#[derive(Debug, Clone)]
pub struct TranscodeConfig {
    pub crf: u8,
    pub preset: String,
    pub use_hardware: bool,
    pub audio_codec: String,
    pub container: String,
}

impl Default for TranscodeConfig {
    fn default() -> Self {
        Self {
            crf: 23,
            preset: "medium".to_string(),
            use_hardware: false,
            audio_codec: "aac".to_string(),
            container: "ts".to_string(),
        }
    }
}

pub struct Transcoder {
    config: TranscodeConfig,
    fingerprinter: Fingerprinter,
}

impl Transcoder {
    pub fn new(config: TranscodeConfig) -> Self {
        Self {
            config,
            fingerprinter: Fingerprinter::new(),
        }
    }

    pub fn transcoding_path(original: &Path, temp_dir: Option<&Path>) -> PathBuf {
        let stem = original.file_stem().unwrap_or_default().to_string_lossy();
        let filename = format!("{}.transcoding", stem);
        match temp_dir {
            Some(dir) => dir.join(filename),
            None => {
                let mut path = original.to_path_buf();
                path.set_file_name(filename);
                path
            }
        }
    }

    pub fn final_path(original: &Path, container: &str) -> PathBuf {
        let mut path = original.to_path_buf();
        path.set_extension(container);
        path
    }

    pub fn transcode(
        &self,
        input: &Path,
        content_start_ms: Option<i32>,
        content_end_ms: Option<i32>,
        temp_dir: Option<&Path>,
    ) -> Result<TranscodeResult> {
        let transcoding_path = Self::transcoding_path(input, temp_dir);
        let final_path = Self::final_path(input, &self.config.container);

        // Build ffmpeg command
        let mut cmd = Command::new("ffmpeg");
        cmd.arg("-y"); // Overwrite output

        // Input seeking (if trimming)
        if let Some(start) = content_start_ms {
            cmd.args(["-ss", &format!("{:.3}", start as f64 / 1000.0)]);
        }

        cmd.args(["-i"]).arg(input);

        // Duration (if trimming)
        if let (Some(start), Some(end)) = (content_start_ms, content_end_ms) {
            let duration = (end - start) as f64 / 1000.0;
            cmd.args(["-t", &format!("{:.3}", duration)]);
        } else if let Some(end) = content_end_ms {
            cmd.args(["-t", &format!("{:.3}", end as f64 / 1000.0)]);
        }

        cmd.args(["-map", "0", "-ignore_unknown"]);
        cmd.args(["-map_metadata", "0"]);
        cmd.args(["-map_chapters", "0"]);

        if self.config.use_hardware {
            cmd.args(["-c:v", "hevc_nvenc", "-preset", "p4", "-cq", &self.config.crf.to_string()]);
        } else {
            cmd.args([
                "-c:v",
                "libx265",
                "-preset",
                &self.config.preset,
                "-crf",
                &self.config.crf.to_string(),
            ]);
        }

        cmd.args(["-c:a", &self.config.audio_codec, "-b:a", "128k"]);
        cmd.args(["-c:s", "copy"]);
        cmd.args(["-c:d", "copy"]);

        // Output format (needed since .transcoding isn't a known extension)
        let format = match self.config.container.as_str() {
            "ts" => "mpegts",
            "mkv" => "matroska",
            "mp4" => "mp4",
            "webm" => "webm",
            other => other,
        };
        cmd.args(["-f", format]);

        // Output
        cmd.arg(&transcoding_path);

        tracing::info!("Transcoding {:?} -> {:?}", input, transcoding_path);

        let output = cmd.output().context("Failed to run ffmpeg")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("ffmpeg failed: {}", stderr);
        }

        // Get sizes
        let original_size = std::fs::metadata(input)?.len() as i64;
        let transcoded_size = std::fs::metadata(&transcoding_path)?.len() as i64;

        Ok(TranscodeResult {
            transcoding_path,
            final_path,
            original_size,
            transcoded_size,
        })
    }

    fn get_fps(&self, path: &Path) -> Result<f64> {
        let output = Command::new("ffprobe")
            .args([
                "-v", "quiet",
                "-select_streams", "v:0",
                "-show_entries", "stream=r_frame_rate",
                "-of", "csv=p=0",
            ])
            .arg(path)
            .output()
            .context("Failed to run ffprobe")?;

        let fps_output = String::from_utf8_lossy(&output.stdout);
        let fps_str = fps_output.lines().next().unwrap_or("30").trim();
        // r_frame_rate is a fraction like "30000/1001"
        if let Some((num, den)) = fps_str.split_once('/') {
            let n: f64 = num.parse().unwrap_or(30.0);
            let d: f64 = den.parse().unwrap_or(1.0);
            Ok(n / d)
        } else {
            Ok(fps_str.parse().unwrap_or(30.0))
        }
    }

    pub fn verify(&self, original: &Path, transcoded: &Path) -> Result<VerifyResult> {
        let duration = self.get_duration(original)?;
        let fps = self.get_fps(original)?;
        let total_frames = (duration as f64 / 1000.0 * fps) as u64;

        // Sample 5 frame numbers spread across the middle 80%
        let start_frame = total_frames / 10;
        let end_frame = total_frames - (total_frames / 10);
        let usable = end_frame - start_frame;
        let interval = usable / 6;
        let frame_numbers: Vec<u64> = (1..=5)
            .map(|i| start_frame + (interval * i))
            .collect();

        tracing::debug!(
            "Verifying: duration={}ms, fps={:.2}, total_frames={}, sample_frames={:?}",
            duration,
            fps,
            total_frames,
            frame_numbers
        );

        let original_hashes = self.fingerprinter.extract_frame_hashes_by_number(original, &frame_numbers)?;
        let transcoded_hashes = self.fingerprinter.extract_frame_hashes_by_number(transcoded, &frame_numbers)?;

        tracing::debug!(
            "Extracted {} original frames, {} transcoded frames",
            original_hashes.len(),
            transcoded_hashes.len()
        );

        if original_hashes.len() != transcoded_hashes.len() {
            return Ok(VerifyResult {
                passed: false,
                similarity: 0.0,
                message: format!(
                    "Different number of frames extracted: {} vs {}",
                    original_hashes.len(),
                    transcoded_hashes.len()
                ),
            });
        }

        let mut total_similarity = 0.0;
        for ((orig_fn, orig_hash), (trans_fn, trans_hash)) in original_hashes.iter().zip(transcoded_hashes.iter()) {
            let sim = Fingerprinter::similarity(orig_hash, trans_hash);
            tracing::debug!(
                "Frame #{} vs #{}: {:.1}% similarity",
                orig_fn,
                trans_fn,
                sim * 100.0
            );
            total_similarity += sim;
        }

        let avg_similarity = if original_hashes.is_empty() {
            0.0
        } else {
            total_similarity / original_hashes.len() as f32
        };

        // Require 95% similarity to pass
        let passed = avg_similarity >= 0.95;

        Ok(VerifyResult {
            passed,
            similarity: avg_similarity,
            message: if passed {
                format!("Verification passed with {:.1}% similarity", avg_similarity * 100.0)
            } else {
                format!(
                    "Verification failed: {:.1}% similarity (need 95%)",
                    avg_similarity * 100.0
                )
            },
        })
    }

    pub fn finalize(&self, result: &TranscodeResult, original: &Path, preserve_original: bool) -> Result<()> {
        if preserve_original && result.final_path == original {
            // Original and output share the same path but we need to keep the original.
            // Rename original to .original.ts, then move transcoded file into place.
            let mut preserved = original.to_path_buf();
            let stem = original.file_stem().unwrap_or_default().to_string_lossy();
            let ext = original.extension().unwrap_or_default().to_string_lossy();
            preserved.set_file_name(format!("{}.original.{}", stem, ext));

            std::fs::rename(original, &preserved)
                .with_context(|| format!("Failed to preserve original as {:?}", preserved))?;
            tracing::info!("Preserved original with unknown streams as {:?}", preserved);
        }

        if let Err(rename_err) = std::fs::rename(&result.transcoding_path, &result.final_path) {
            tracing::debug!(
                "Rename failed ({}), falling back to copy+delete with integrity check",
                rename_err
            );

            let source_hash = hash_file(&result.transcoding_path)
                .context("Failed to hash temp file before copy")?;

            std::fs::copy(&result.transcoding_path, &result.final_path)
                .context("Failed to copy transcoded file to final location")?;

            let dest_hash = hash_file(&result.final_path)
                .context("Failed to hash copied file")?;

            if source_hash != dest_hash {
                let _ = std::fs::remove_file(&result.final_path);
                anyhow::bail!(
                    "Copy integrity check failed: {} != {}",
                    source_hash.to_hex(),
                    dest_hash.to_hex()
                );
            }

            tracing::debug!("Copy integrity verified (blake3: {})", source_hash.to_hex());

            std::fs::remove_file(&result.transcoding_path)
                .context("Failed to remove temporary transcoded file")?;
        }

        if !preserve_original && result.final_path != original {
            std::fs::remove_file(original).context("Failed to delete original file")?;
        }

        tracing::info!(
            "Finalized: {:?} (saved {} bytes, {:.1}% reduction)",
            result.final_path,
            result.original_size - result.transcoded_size,
            (1.0 - (result.transcoded_size as f64 / result.original_size as f64)) * 100.0
        );

        Ok(())
    }

    pub fn cleanup_failed(&self, transcoding_path: &Path) -> Result<()> {
        if transcoding_path.exists() {
            std::fs::remove_file(transcoding_path).context("Failed to cleanup transcoding file")?;
        }
        Ok(())
    }

    fn get_duration(&self, path: &Path) -> Result<i32> {
        let output = Command::new("ffprobe")
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_format",
            ])
            .arg(path)
            .output()
            .context("Failed to run ffprobe")?;

        let probe: serde_json::Value =
            serde_json::from_slice(&output.stdout).context("Failed to parse ffprobe output")?;

        let duration_str = probe["format"]["duration"]
            .as_str()
            .context("No duration in ffprobe output")?;

        let duration_secs: f64 = duration_str.parse().context("Invalid duration")?;
        Ok((duration_secs * 1000.0) as i32)
    }
}

#[derive(Debug)]
pub struct TranscodeResult {
    pub transcoding_path: PathBuf,
    pub final_path: PathBuf,
    pub original_size: i64,
    pub transcoded_size: i64,
}

impl TranscodeResult {
    pub fn savings_bytes(&self) -> i64 {
        self.original_size - self.transcoded_size
    }

    pub fn savings_percent(&self) -> f64 {
        if self.original_size == 0 {
            0.0
        } else {
            (1.0 - (self.transcoded_size as f64 / self.original_size as f64)) * 100.0
        }
    }
}

/// Extract embedded subtitles to SRT file next to video.
pub fn extract_subtitles(video_path: &Path) -> Result<Option<PathBuf>> {
    let srt_path = video_path.with_extension("srt");

    if srt_path.exists() {
        return Ok(Some(srt_path));
    }

    let probe = Command::new("ffprobe")
        .args([
            "-v", "quiet",
            "-select_streams", "s",
            "-show_entries", "stream=index,codec_name",
            "-of", "json",
        ])
        .arg(video_path)
        .output()
        .context("Failed to run ffprobe")?;

    let probe_json: serde_json::Value = serde_json::from_slice(&probe.stdout)
        .unwrap_or(serde_json::json!({"streams": []}));

    let streams = probe_json["streams"].as_array();
    if streams.map(|s| s.is_empty()).unwrap_or(true) {
        tracing::debug!("No subtitle streams found in {:?}", video_path);
        return Ok(None);
    }

    let output = Command::new("ffmpeg")
        .args(["-y", "-i"])
        .arg(video_path)
        .args(["-map", "0:s:0", "-c:s", "srt"])
        .arg(&srt_path)
        .output()
        .context("Failed to extract subtitles")?;

    if output.status.success() && srt_path.exists() {
        tracing::info!("Extracted subtitles to {:?}", srt_path);
        Ok(Some(srt_path))
    } else {
        tracing::debug!("Could not extract subtitles from {:?}", video_path);
        Ok(None)
    }
}

#[derive(Debug)]
pub struct VerifyResult {
    pub passed: bool,
    pub similarity: f32,
    pub message: String,
}

fn hash_file(path: &Path) -> Result<blake3::Hash> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("Failed to open {:?} for hashing", path))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(file)
        .context("Failed to read file during hashing")?;
    Ok(hasher.finalize())
}

pub fn detect_hardware_encoder() -> Option<&'static str> {
    // Check for NVENC
    let nvenc = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .ok()?;

    let encoders = String::from_utf8_lossy(&nvenc.stdout);
    if encoders.contains("hevc_nvenc") {
        return Some("nvenc");
    }
    if encoders.contains("hevc_vaapi") {
        return Some("vaapi");
    }
    if encoders.contains("hevc_qsv") {
        return Some("qsv");
    }

    None
}
