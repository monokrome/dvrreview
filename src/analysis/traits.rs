use anyhow::Result;
use std::path::Path;

/// A point in a fingerprint timeline
#[derive(Debug, Clone)]
pub struct FingerprintSample {
    pub timestamp_ms: i32,
    pub hash: Vec<u8>,
}

/// A dense fingerprint timeline for a piece of content
#[derive(Debug, Clone)]
pub struct FingerprintTimeline {
    pub samples: Vec<FingerprintSample>,
    pub sample_interval_ms: i32,
}

impl FingerprintTimeline {
    pub fn duration_ms(&self) -> i32 {
        self.samples.last().map(|s| s.timestamp_ms).unwrap_or(0)
    }

    pub fn sample_at(&self, timestamp_ms: i32) -> Option<&FingerprintSample> {
        self.samples
            .iter()
            .min_by_key(|s| (s.timestamp_ms - timestamp_ms).abs())
    }
}

/// A detected segment within content
#[derive(Debug, Clone)]
pub struct Segment {
    pub start_ms: i32,
    pub end_ms: i32,
    pub segment_type: SegmentType,
    pub confidence: f32,
    pub metadata: SegmentMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentType {
    Content,
    Commercial,
    Intro,
    Outro,
    Recap,
    Preview,
    Unknown,
}

#[derive(Debug, Clone, Default)]
pub struct SegmentMetadata {
    /// Which source(s) contributed to detecting this segment
    pub sources: Vec<String>,
    /// Quality score if evaluated
    pub quality_score: Option<f32>,
    /// Additional key-value data
    pub extra: std::collections::HashMap<String, String>,
}

/// Alignment between two timelines
#[derive(Debug, Clone)]
pub struct TimelineAlignment {
    /// Offset to add to timeline B to align with timeline A
    pub offset_ms: i32,
    /// Confidence in the alignment (0.0 - 1.0)
    pub confidence: f32,
    /// Matched sample pairs (index_a, index_b, similarity)
    pub matched_samples: Vec<(usize, usize, f32)>,
}

/// Quality assessment for a segment or frame
#[derive(Debug, Clone)]
pub struct QualityAssessment {
    pub overall_score: f32,
    pub metrics: QualityMetrics,
}

#[derive(Debug, Clone, Default)]
pub struct QualityMetrics {
    /// Watermark severity (0.0 = none, 1.0 = severe)
    pub watermark_score: Option<f32>,
    /// Compression artifact severity
    pub artifact_score: Option<f32>,
    /// Brightness/contrast quality
    pub exposure_score: Option<f32>,
    /// Sharpness/blur assessment
    pub sharpness_score: Option<f32>,
}

// =============================================================================
// Core traits for fingerprint-based operations
// =============================================================================

/// Extracts dense fingerprint timelines from video files
pub trait TimelineExtractor {
    fn extract(&self, video_path: &Path, interval_ms: i32) -> Result<FingerprintTimeline>;
}

/// Aligns two fingerprint timelines to find temporal offset
pub trait TimelineAligner {
    fn align(&self, timeline_a: &FingerprintTimeline, timeline_b: &FingerprintTimeline) -> Result<TimelineAlignment>;
}

/// Detects segment boundaries within content
pub trait SegmentDetector {
    fn detect(&self, context: &SegmentDetectionContext) -> Result<Vec<Segment>>;
}

/// Context provided to segment detectors
#[derive(Debug)]
pub struct SegmentDetectionContext<'a> {
    /// The primary timeline being analyzed
    pub primary: &'a FingerprintTimeline,
    /// Other timelines of the same content (for comparison-based detection)
    pub comparisons: Vec<&'a FingerprintTimeline>,
    /// Alignments between primary and comparison timelines
    pub alignments: Vec<&'a TimelineAlignment>,
}

/// Scores segment quality to help choose best source
pub trait SegmentScorer {
    fn score(&self, video_path: &Path, segment: &Segment) -> Result<QualityAssessment>;
}

/// Determines if two fingerprint sets represent the same content
pub trait ContentMatcher {
    fn matches(&self, timeline_a: &FingerprintTimeline, timeline_b: &FingerprintTimeline) -> Result<ContentMatch>;
}

#[derive(Debug, Clone)]
pub struct ContentMatch {
    pub is_match: bool,
    pub confidence: f32,
    pub alignment: Option<TimelineAlignment>,
}

/// Generates output from analyzed content (chapters, EDL, composite video)
pub trait ContentRenderer {
    fn render(&self, context: &RenderContext) -> Result<RenderOutput>;
}

#[derive(Debug)]
pub struct RenderContext<'a> {
    pub segments: &'a [Segment],
    pub sources: Vec<RenderSource<'a>>,
    pub options: RenderOptions,
}

#[derive(Debug)]
pub struct RenderSource<'a> {
    pub path: &'a Path,
    pub timeline: &'a FingerprintTimeline,
    pub alignment: Option<&'a TimelineAlignment>,
}

#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    /// Include commercials in output (marked as chapters)
    pub include_commercials: bool,
    /// Add chapter markers to output
    pub add_chapters: bool,
    /// Composite from multiple sources or use single best
    pub composite_sources: bool,
}

#[derive(Debug)]
pub enum RenderOutput {
    /// Chapter metadata to mux into existing file
    Chapters(Vec<Chapter>),
    /// EDL file content
    Edl(String),
    /// Path to newly created composite file
    CompositeFile(std::path::PathBuf),
}

#[derive(Debug, Clone)]
pub struct Chapter {
    pub start_ms: i32,
    pub end_ms: i32,
    pub title: String,
    pub is_content: bool,
}
