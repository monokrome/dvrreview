use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct MediaMetadata {
    pub duration_ms: Option<i32>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub bitrate: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct FfprobeOutput {
    format: Option<FfprobeFormat>,
    streams: Option<Vec<FfprobeStream>>,
}

#[derive(Debug, Deserialize)]
struct FfprobeFormat {
    duration: Option<String>,
    bit_rate: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<i32>,
    height: Option<i32>,
}

impl MediaMetadata {
    pub fn extract(path: &Path) -> Result<Self> {
        let output = Command::new("ffprobe")
            .args([
                "-v", "quiet",
                "-print_format", "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(path)
            .output()
            .context("Failed to run ffprobe")?;

        if !output.status.success() {
            anyhow::bail!(
                "ffprobe failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let probe: FfprobeOutput =
            serde_json::from_slice(&output.stdout).context("Failed to parse ffprobe output")?;

        let mut metadata = MediaMetadata {
            duration_ms: None,
            video_codec: None,
            audio_codec: None,
            width: None,
            height: None,
            bitrate: None,
        };

        if let Some(format) = probe.format {
            if let Some(duration_str) = format.duration {
                if let Ok(duration_secs) = duration_str.parse::<f64>() {
                    metadata.duration_ms = Some((duration_secs * 1000.0) as i32);
                }
            }
            if let Some(bitrate_str) = format.bit_rate {
                if let Ok(bitrate) = bitrate_str.parse::<i32>() {
                    metadata.bitrate = Some(bitrate);
                }
            }
        }

        if let Some(streams) = probe.streams {
            for stream in streams {
                match stream.codec_type.as_deref() {
                    Some("video") if metadata.video_codec.is_none() => {
                        metadata.video_codec = stream.codec_name;
                        metadata.width = stream.width;
                        metadata.height = stream.height;
                    }
                    Some("audio") if metadata.audio_codec.is_none() => {
                        metadata.audio_codec = stream.codec_name;
                    }
                    _ => {}
                }
            }
        }

        Ok(metadata)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ParsedFilename {
    pub title: Option<String>,
    pub season: Option<i32>,
    pub episode: Option<i32>,
}

pub fn parse_filename(filename: &str) -> ParsedFilename {
    let stem = Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);

    let mut parsed = ParsedFilename::default();

    // HDHomeRun DVR typically uses formats like:
    // "Show Name S01E02 - Episode Title"
    // "Show Name 2024-01-15"
    // "Movie Name (2024)"

    // Try to extract season/episode pattern
    let se_pattern = regex_lite::Regex::new(r"[Ss](\d{1,2})[Ee](\d{1,2})").ok();
    if let Some(re) = se_pattern {
        if let Some(caps) = re.captures(stem) {
            parsed.season = caps.get(1).and_then(|m| m.as_str().parse().ok());
            parsed.episode = caps.get(2).and_then(|m| m.as_str().parse().ok());

            // Title is everything before the S01E02 pattern
            if let Some(m) = re.find(stem) {
                let title_part = &stem[..m.start()];
                let title = title_part.trim().trim_end_matches(['-', '_', '.']);
                if !title.is_empty() {
                    parsed.title = Some(title.replace(['.', '_'], " ").trim().to_string());
                }
            }
        }
    }

    // If no S01E02 pattern, use the whole stem as title (cleaned up)
    if parsed.title.is_none() {
        let cleaned = stem
            .replace(['.', '_'], " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !cleaned.is_empty() {
            parsed.title = Some(cleaned);
        }
    }

    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_filename_with_season_episode() {
        let parsed = parse_filename("SpongeBob SquarePants S03E12 - Chocolate With Nuts.mkv");
        assert_eq!(parsed.title, Some("SpongeBob SquarePants".to_string()));
        assert_eq!(parsed.season, Some(3));
        assert_eq!(parsed.episode, Some(12));
    }

    #[test]
    fn test_parse_filename_no_episode() {
        let parsed = parse_filename("Some Movie (2024).mkv");
        assert_eq!(parsed.title, Some("Some Movie (2024)".to_string()));
        assert_eq!(parsed.season, None);
        assert_eq!(parsed.episode, None);
    }
}
