use super::traits::*;
use crate::scanner::fingerprint::Fingerprinter;
use anyhow::{Context, Result};
use std::path::Path;

/// Extracts dense fingerprint timelines using the existing Fingerprinter
pub struct DenseTimelineExtractor {
    fingerprinter: Fingerprinter,
}

impl Default for DenseTimelineExtractor {
    fn default() -> Self {
        Self {
            fingerprinter: Fingerprinter::new(),
        }
    }
}

impl TimelineExtractor for DenseTimelineExtractor {
    fn extract(&self, video_path: &Path, interval_ms: i32) -> Result<FingerprintTimeline> {
        let duration = get_video_duration(video_path)?;
        let timestamps: Vec<i32> = (0..duration).step_by(interval_ms as usize).collect();

        let hashes = self.fingerprinter.extract_frame_hashes(video_path, &timestamps)?;

        let samples = hashes
            .into_iter()
            .map(|(ts, hash)| FingerprintSample {
                timestamp_ms: ts,
                hash,
            })
            .collect();

        Ok(FingerprintTimeline {
            samples,
            sample_interval_ms: interval_ms,
        })
    }
}

/// Aligns timelines using cross-correlation of fingerprint hashes
pub struct CrossCorrelationAligner {
    /// Maximum offset to search (in either direction)
    pub max_offset_ms: i32,
    /// Step size for searching offsets
    pub search_step_ms: i32,
    /// Minimum similarity to consider samples matching
    pub match_threshold: f32,
}

impl Default for CrossCorrelationAligner {
    fn default() -> Self {
        Self {
            max_offset_ms: 300_000, // 5 minutes max offset
            search_step_ms: 1000,   // 1 second steps
            match_threshold: 0.85,
        }
    }
}

impl TimelineAligner for CrossCorrelationAligner {
    fn align(&self, timeline_a: &FingerprintTimeline, timeline_b: &FingerprintTimeline) -> Result<TimelineAlignment> {
        let mut best_offset = 0;
        let mut best_score = 0.0;
        let mut best_matches = Vec::new();

        // Try different offsets
        let mut offset = -self.max_offset_ms;
        while offset <= self.max_offset_ms {
            let (score, matches) = self.score_alignment(timeline_a, timeline_b, offset);

            if score > best_score {
                best_score = score;
                best_offset = offset;
                best_matches = matches;
            }

            offset += self.search_step_ms;
        }

        // Refine around the best offset
        let refined_offset = self.refine_offset(timeline_a, timeline_b, best_offset);

        Ok(TimelineAlignment {
            offset_ms: refined_offset,
            confidence: best_score,
            matched_samples: best_matches,
        })
    }
}

impl CrossCorrelationAligner {
    fn score_alignment(
        &self,
        timeline_a: &FingerprintTimeline,
        timeline_b: &FingerprintTimeline,
        offset: i32,
    ) -> (f32, Vec<(usize, usize, f32)>) {
        let mut matches = Vec::new();
        let mut total_similarity = 0.0;
        let mut count = 0;

        for (i, sample_a) in timeline_a.samples.iter().enumerate() {
            let target_ts = sample_a.timestamp_ms + offset;

            if let Some((j, sample_b)) = timeline_b
                .samples
                .iter()
                .enumerate()
                .min_by_key(|(_, s)| (s.timestamp_ms - target_ts).abs())
            {
                // Only count if within reasonable range of target
                if (sample_b.timestamp_ms - target_ts).abs() <= timeline_b.sample_interval_ms {
                    let similarity = hash_similarity(&sample_a.hash, &sample_b.hash);
                    total_similarity += similarity;
                    count += 1;

                    if similarity >= self.match_threshold {
                        matches.push((i, j, similarity));
                    }
                }
            }
        }

        let score = if count > 0 {
            total_similarity / count as f32
        } else {
            0.0
        };

        (score, matches)
    }

    fn refine_offset(&self, timeline_a: &FingerprintTimeline, timeline_b: &FingerprintTimeline, coarse_offset: i32) -> i32 {
        // Fine-grained search around the coarse offset
        let search_range = self.search_step_ms;
        let fine_step = timeline_a.sample_interval_ms.max(100);

        let mut best_offset = coarse_offset;
        let mut best_score = 0.0;

        let mut offset = coarse_offset - search_range;
        while offset <= coarse_offset + search_range {
            let (score, _) = self.score_alignment(timeline_a, timeline_b, offset);
            if score > best_score {
                best_score = score;
                best_offset = offset;
            }
            offset += fine_step;
        }

        best_offset
    }
}

/// Matches content by comparing fingerprint timelines
pub struct FingerprintContentMatcher {
    pub aligner: CrossCorrelationAligner,
    /// Minimum percentage of samples that must match
    pub min_match_ratio: f32,
    /// Similarity threshold for individual samples
    pub similarity_threshold: f32,
}

impl Default for FingerprintContentMatcher {
    fn default() -> Self {
        Self {
            aligner: CrossCorrelationAligner::default(),
            min_match_ratio: 0.6,
            similarity_threshold: 0.85,
        }
    }
}

impl ContentMatcher for FingerprintContentMatcher {
    fn matches(&self, timeline_a: &FingerprintTimeline, timeline_b: &FingerprintTimeline) -> Result<ContentMatch> {
        let alignment = self.aligner.align(timeline_a, timeline_b)?;

        let match_ratio = alignment.matched_samples.len() as f32
            / timeline_a.samples.len().min(timeline_b.samples.len()) as f32;

        let is_match = match_ratio >= self.min_match_ratio && alignment.confidence >= self.similarity_threshold;

        Ok(ContentMatch {
            is_match,
            confidence: alignment.confidence * match_ratio,
            alignment: Some(alignment),
        })
    }
}

/// Types of recognizable patterns
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternType {
    Commercial,
    Intro,
    Outro,
    Recap,
    Preview,
}

/// Database of known pattern fingerprints for recognition
#[derive(Debug, Clone)]
pub struct KnownPattern {
    pub id: uuid::Uuid,
    pub pattern_type: PatternType,
    pub name: Option<String>,
    /// For series-specific patterns (intros, outros, recaps)
    pub series_title: Option<String>,
    pub duration_ms: i32,
    pub timeline: FingerprintTimeline,
    pub detection_count: i32,
    pub first_seen: chrono::DateTime<chrono::Utc>,
    pub last_seen: chrono::DateTime<chrono::Utc>,
}

/// Detects patterns by matching against a database of known patterns
pub struct PatternDetector {
    pub matcher: FingerprintContentMatcher,
    /// Minimum duration to consider a potential pattern
    pub min_duration_ms: i32,
    /// Maximum duration for a single pattern
    pub max_duration_ms: i32,
}

impl Default for PatternDetector {
    fn default() -> Self {
        Self {
            matcher: FingerprintContentMatcher::default(),
            min_duration_ms: 5_000,   // 5 seconds
            max_duration_ms: 300_000, // 5 minutes (intros can be longer than commercials)
        }
    }
}

impl PatternDetector {
    /// Searches for known patterns within a timeline
    pub fn find_patterns(
        &self,
        timeline: &FingerprintTimeline,
        known: &[KnownPattern],
    ) -> Result<Vec<PatternMatch>> {
        let mut matches = Vec::new();

        for pattern in known {
            if let Some(m) = self.find_pattern_in_timeline(timeline, pattern)? {
                matches.push(m);
            }
        }

        // Sort by position and remove overlaps
        matches.sort_by_key(|m| m.start_ms);
        Ok(self.remove_overlapping_matches(matches))
    }

    /// Searches for patterns of a specific type only
    pub fn find_patterns_of_type(
        &self,
        timeline: &FingerprintTimeline,
        known: &[KnownPattern],
        pattern_type: &PatternType,
    ) -> Result<Vec<PatternMatch>> {
        let filtered: Vec<_> = known.iter()
            .filter(|p| &p.pattern_type == pattern_type)
            .cloned()
            .collect();
        self.find_patterns(timeline, &filtered)
    }

    /// Searches for series-specific patterns (intros, outros, etc.)
    pub fn find_series_patterns(
        &self,
        timeline: &FingerprintTimeline,
        known: &[KnownPattern],
        series_title: &str,
    ) -> Result<Vec<PatternMatch>> {
        let filtered: Vec<_> = known.iter()
            .filter(|p| p.series_title.as_deref() == Some(series_title))
            .cloned()
            .collect();
        self.find_patterns(timeline, &filtered)
    }

    fn find_pattern_in_timeline(
        &self,
        timeline: &FingerprintTimeline,
        pattern: &KnownPattern,
    ) -> Result<Option<PatternMatch>> {
        let content_duration = timeline.duration_ms();
        let pattern_duration = pattern.duration_ms;

        if pattern_duration > content_duration {
            return Ok(None);
        }

        let mut best_match: Option<PatternMatch> = None;
        let step = timeline.sample_interval_ms;

        let mut position = 0;
        while position + pattern_duration <= content_duration {
            let window = self.extract_window(timeline, position, pattern_duration);
            let content_match = self.matcher.matches(&window, &pattern.timeline)?;

            if content_match.is_match {
                let new_match = PatternMatch {
                    pattern_id: pattern.id,
                    pattern_type: pattern.pattern_type.clone(),
                    start_ms: position,
                    end_ms: position + pattern_duration,
                    confidence: content_match.confidence,
                };

                if best_match.as_ref().map_or(true, |m| content_match.confidence > m.confidence) {
                    best_match = Some(new_match);
                }
            }

            position += step;
        }

        Ok(best_match)
    }

    fn extract_window(&self, timeline: &FingerprintTimeline, start_ms: i32, duration_ms: i32) -> FingerprintTimeline {
        let end_ms = start_ms + duration_ms;
        let samples: Vec<_> = timeline
            .samples
            .iter()
            .filter(|s| s.timestamp_ms >= start_ms && s.timestamp_ms < end_ms)
            .map(|s| FingerprintSample {
                timestamp_ms: s.timestamp_ms - start_ms,
                hash: s.hash.clone(),
            })
            .collect();

        FingerprintTimeline {
            samples,
            sample_interval_ms: timeline.sample_interval_ms,
        }
    }

    fn remove_overlapping_matches(&self, matches: Vec<PatternMatch>) -> Vec<PatternMatch> {
        let mut result = Vec::new();

        for m in matches {
            let overlaps = result.iter().any(|existing: &PatternMatch| {
                m.start_ms < existing.end_ms && m.end_ms > existing.start_ms
            });

            if !overlaps {
                result.push(m);
            }
        }

        result
    }

    /// Extracts a new pattern fingerprint from detected segment
    pub fn extract_pattern(
        &self,
        timeline: &FingerprintTimeline,
        start_ms: i32,
        end_ms: i32,
        pattern_type: PatternType,
        series_title: Option<String>,
    ) -> KnownPattern {
        let pattern_timeline = self.extract_window(timeline, start_ms, end_ms - start_ms);
        let now = chrono::Utc::now();

        KnownPattern {
            id: uuid::Uuid::new_v4(),
            pattern_type,
            name: None,
            series_title,
            duration_ms: end_ms - start_ms,
            timeline: pattern_timeline,
            detection_count: 1,
            first_seen: now,
            last_seen: now,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PatternMatch {
    pub pattern_id: uuid::Uuid,
    pub pattern_type: PatternType,
    pub start_ms: i32,
    pub end_ms: i32,
    pub confidence: f32,
}

fn get_video_duration(path: &Path) -> Result<i32> {
    use std::process::Command;

    let output = Command::new("ffprobe")
        .args(["-v", "quiet", "-print_format", "json", "-show_format"])
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

fn hash_similarity(a: &[u8], b: &[u8]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let distance: u32 = a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x ^ y).count_ones())
        .sum();
    let max_distance = (a.len() * 8) as f32;
    1.0 - (distance as f32 / max_distance)
}
