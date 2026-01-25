use anyhow::{Context, Result};
use img_hash::{HashAlg, HasherConfig};
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

pub struct Fingerprinter {
    hasher: img_hash::Hasher<Box<[u8]>>,
}

impl Default for Fingerprinter {
    fn default() -> Self {
        Self::new()
    }
}

impl Fingerprinter {
    pub fn new() -> Self {
        let hasher = HasherConfig::new()
            .hash_alg(HashAlg::DoubleGradient)
            .hash_size(8, 8)
            .to_hasher();
        Self { hasher }
    }

    pub fn extract_frame_hashes(
        &self,
        video_path: &Path,
        timestamps_ms: &[i32],
    ) -> Result<Vec<(i32, Vec<u8>)>> {
        let temp_dir = TempDir::new().context("Failed to create temp directory")?;
        let mut results = Vec::new();

        for &ts_ms in timestamps_ms {
            let ts_secs = ts_ms as f64 / 1000.0;
            let frame_path = temp_dir.path().join(format!("frame_{}.png", ts_ms));

            let status = Command::new("ffmpeg")
                .args(["-ss", &format!("{:.3}", ts_secs), "-i"])
                .arg(video_path)
                .args([
                    "-vframes",
                    "1",
                    "-vf",
                    "crop=iw*0.8:ih*0.8:iw*0.1:ih*0.1",
                    "-y",
                ])
                .arg(&frame_path)
                .stderr(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .output()
                .context("Failed to run ffmpeg")?;

            if !status.status.success() {
                tracing::warn!(
                    "Failed to extract frame at {}ms from {:?}",
                    ts_ms,
                    video_path
                );
                continue;
            }

            // Use img_hash's bundled image crate
            if let Ok(img) = img_hash::image::open(&frame_path) {
                let hash = self.hasher.hash_image(&img);
                results.push((ts_ms, hash.as_bytes().to_vec()));
            }
        }

        Ok(results)
    }

    pub fn hamming_distance(a: &[u8], b: &[u8]) -> u32 {
        if a.len() != b.len() {
            return u32::MAX;
        }
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| (x ^ y).count_ones())
            .sum()
    }

    pub fn similarity(a: &[u8], b: &[u8]) -> f32 {
        let distance = Self::hamming_distance(a, b);
        let max_distance = (a.len() * 8) as f32;
        1.0 - (distance as f32 / max_distance)
    }
}

pub fn generate_sample_timestamps(duration_ms: i32, count: usize) -> Vec<i32> {
    if duration_ms <= 0 || count == 0 {
        return vec![];
    }

    // Skip first and last 10% to avoid commercials
    let start = duration_ms / 10;
    let end = duration_ms - (duration_ms / 10);
    let usable_duration = end - start;

    if usable_duration <= 0 {
        return vec![duration_ms / 2];
    }

    let interval = usable_duration / (count as i32 + 1);
    (1..=count as i32)
        .map(|i| start + (interval * i))
        .collect()
}

pub fn generate_thumbnail_timestamps(duration_ms: i32, count: usize) -> Vec<i32> {
    if duration_ms <= 0 || count == 0 {
        return vec![];
    }

    // For thumbnails, spread across the whole video including edges
    let interval = duration_ms / (count as i32 + 1);
    (1..=count as i32).map(|i| interval * i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sample_timestamps() {
        let timestamps = generate_sample_timestamps(100_000, 5);
        assert_eq!(timestamps.len(), 5);
        for ts in &timestamps {
            assert!(*ts >= 10_000);
            assert!(*ts <= 90_000);
        }
    }

    #[test]
    fn test_hamming_distance() {
        let a = vec![0b00000000];
        let b = vec![0b11111111];
        assert_eq!(Fingerprinter::hamming_distance(&a, &b), 8);

        let c = vec![0b10101010];
        let d = vec![0b01010101];
        assert_eq!(Fingerprinter::hamming_distance(&c, &d), 8);

        let e = vec![0b11110000];
        let f = vec![0b11110000];
        assert_eq!(Fingerprinter::hamming_distance(&e, &f), 0);
    }
}
