use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::scanner::fingerprint::Fingerprinter;

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
            container: "mkv".to_string(),
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

    pub fn transcoding_path(original: &Path) -> PathBuf {
        let mut path = original.to_path_buf();
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        path.set_file_name(format!("{}.transcoding", stem));
        path
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
    ) -> Result<TranscodeResult> {
        let transcoding_path = Self::transcoding_path(input);
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

        // Video codec
        if self.config.use_hardware {
            // Try NVENC first
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

        // Audio codec
        cmd.args(["-c:a", &self.config.audio_codec, "-b:a", "128k"]);

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

    pub fn verify(&self, original: &Path, transcoded: &Path) -> Result<VerifyResult> {
        // Sample 5 frames from each and compare hashes
        let duration = self.get_duration(original)?;
        let timestamps = crate::scanner::fingerprint::generate_sample_timestamps(duration, 5);

        let original_hashes = self.fingerprinter.extract_frame_hashes(original, &timestamps)?;
        let transcoded_hashes = self.fingerprinter.extract_frame_hashes(transcoded, &timestamps)?;

        if original_hashes.len() != transcoded_hashes.len() {
            return Ok(VerifyResult {
                passed: false,
                similarity: 0.0,
                message: "Different number of frames extracted".to_string(),
            });
        }

        let mut total_similarity = 0.0;
        for ((_, orig_hash), (_, trans_hash)) in original_hashes.iter().zip(transcoded_hashes.iter()) {
            total_similarity += Fingerprinter::similarity(orig_hash, trans_hash);
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

    pub fn finalize(&self, result: &TranscodeResult, original: &Path) -> Result<()> {
        // Rename transcoding file to final name
        std::fs::rename(&result.transcoding_path, &result.final_path)
            .context("Failed to rename transcoded file")?;

        // Delete original
        std::fs::remove_file(original).context("Failed to delete original file")?;

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

#[derive(Debug)]
pub struct VerifyResult {
    pub passed: bool,
    pub similarity: f32,
    pub message: String,
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
