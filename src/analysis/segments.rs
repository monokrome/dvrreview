use super::traits::*;
use anyhow::Result;

/// Detects segments by comparing multiple recordings of the same content.
/// Segments where fingerprints match = content, where they diverge = commercials.
pub struct DuplicateComparisonDetector {
    /// Minimum similarity to consider samples matching
    pub match_threshold: f32,
    /// Minimum duration to consider a segment (avoids tiny false positives)
    pub min_segment_ms: i32,
}

impl Default for DuplicateComparisonDetector {
    fn default() -> Self {
        Self {
            match_threshold: 0.85,
            min_segment_ms: 5000, // 5 seconds minimum
        }
    }
}

impl SegmentDetector for DuplicateComparisonDetector {
    fn detect(&self, context: &SegmentDetectionContext) -> Result<Vec<Segment>> {
        if context.comparisons.is_empty() {
            // Can't do comparison-based detection without comparisons
            return Ok(vec![]);
        }

        let mut segments = Vec::new();
        let mut current_type = SegmentType::Unknown;
        let mut segment_start = 0i32;

        for sample in &context.primary.samples {
            let mut match_count = 0;
            let mut total_comparisons = 0;

            for (comp_timeline, alignment) in context.comparisons.iter().zip(context.alignments.iter()) {
                let adjusted_ts = sample.timestamp_ms - alignment.offset_ms;

                if let Some(comp_sample) = comp_timeline.sample_at(adjusted_ts) {
                    let similarity = hash_similarity(&sample.hash, &comp_sample.hash);
                    if similarity >= self.match_threshold {
                        match_count += 1;
                    }
                    total_comparisons += 1;
                }
            }

            let is_content = total_comparisons > 0 && match_count == total_comparisons;
            let detected_type = if is_content {
                SegmentType::Content
            } else {
                SegmentType::Commercial
            };

            if detected_type != current_type {
                // Segment boundary
                if current_type != SegmentType::Unknown && sample.timestamp_ms - segment_start >= self.min_segment_ms {
                    segments.push(Segment {
                        start_ms: segment_start,
                        end_ms: sample.timestamp_ms,
                        segment_type: current_type.clone(),
                        confidence: 0.8, // Could calculate based on match ratios
                        metadata: SegmentMetadata::default(),
                    });
                }
                current_type = detected_type;
                segment_start = sample.timestamp_ms;
            }
        }

        // Close final segment
        if current_type != SegmentType::Unknown {
            let end = context.primary.duration_ms();
            if end - segment_start >= self.min_segment_ms {
                segments.push(Segment {
                    start_ms: segment_start,
                    end_ms: end,
                    segment_type: current_type,
                    confidence: 0.8,
                    metadata: SegmentMetadata::default(),
                });
            }
        }

        Ok(segments)
    }
}

/// Detects segments by looking for scene changes within a single recording.
/// Useful as a fallback when no duplicates exist.
pub struct SceneChangeDetector {
    /// Similarity threshold below which we consider it a scene change
    pub change_threshold: f32,
    /// Minimum time between detected changes
    pub min_gap_ms: i32,
}

impl Default for SceneChangeDetector {
    fn default() -> Self {
        Self {
            change_threshold: 0.7,
            min_gap_ms: 1000,
        }
    }
}

impl SceneChangeDetector {
    pub fn detect_changes(&self, timeline: &FingerprintTimeline) -> Vec<i32> {
        let mut changes = Vec::new();
        let mut last_change = 0i32;

        for window in timeline.samples.windows(2) {
            let similarity = hash_similarity(&window[0].hash, &window[1].hash);

            if similarity < self.change_threshold {
                let ts = window[1].timestamp_ms;
                if ts - last_change >= self.min_gap_ms {
                    changes.push(ts);
                    last_change = ts;
                }
            }
        }

        changes
    }
}

/// Combines multiple detectors, using the best available method
pub struct AdaptiveSegmentDetector {
    pub duplicate_detector: DuplicateComparisonDetector,
    pub scene_detector: SceneChangeDetector,
}

impl Default for AdaptiveSegmentDetector {
    fn default() -> Self {
        Self {
            duplicate_detector: DuplicateComparisonDetector::default(),
            scene_detector: SceneChangeDetector::default(),
        }
    }
}

impl SegmentDetector for AdaptiveSegmentDetector {
    fn detect(&self, context: &SegmentDetectionContext) -> Result<Vec<Segment>> {
        if !context.comparisons.is_empty() {
            // Have duplicates - use comparison-based detection
            self.duplicate_detector.detect(context)
        } else {
            // No duplicates - fall back to scene detection
            // This is less reliable for commercial detection but can still
            // identify potential break points
            let changes = self.scene_detector.detect_changes(context.primary);

            // Convert scene changes to segments (all marked as unknown/content
            // since we can't distinguish without comparison)
            let mut segments = Vec::new();
            let mut start = 0;

            for change in changes {
                segments.push(Segment {
                    start_ms: start,
                    end_ms: change,
                    segment_type: SegmentType::Content, // Assume content without comparison
                    confidence: 0.5, // Low confidence without comparison
                    metadata: SegmentMetadata::default(),
                });
                start = change;
            }

            // Final segment
            if start < context.primary.duration_ms() {
                segments.push(Segment {
                    start_ms: start,
                    end_ms: context.primary.duration_ms(),
                    segment_type: SegmentType::Content,
                    confidence: 0.5,
                    metadata: SegmentMetadata::default(),
                });
            }

            Ok(segments)
        }
    }
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
