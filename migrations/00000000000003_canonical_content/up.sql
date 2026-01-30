-- Canonical content: the "perfect" representation of a piece of content
CREATE TABLE canonical_content (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    dvr_id UUID NOT NULL REFERENCES dvrs(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    season INTEGER,
    episode INTEGER,
    duration_ms INTEGER NOT NULL,
    -- Path to the current best composite file
    file_path TEXT,
    -- Dense fingerprint timeline stored as binary blob
    -- Format: [timestamp_ms (i32), hash_len (u8), hash (bytes)]...
    fingerprint_timeline BYTEA,
    fingerprint_interval_ms INTEGER,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW()
);

-- Segments within canonical content (content vs commercial boundaries)
CREATE TABLE canonical_segments (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    content_id UUID NOT NULL REFERENCES canonical_content(id) ON DELETE CASCADE,
    start_ms INTEGER NOT NULL,
    end_ms INTEGER NOT NULL,
    -- 'content', 'commercial', 'intro', 'outro', 'recap', 'preview'
    segment_type TEXT NOT NULL,
    confidence REAL NOT NULL,
    -- JSON metadata (quality scores, source info, etc.)
    metadata JSONB,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW()
);

-- Sources that contributed to a canonical content entry
CREATE TABLE canonical_sources (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    content_id UUID NOT NULL REFERENCES canonical_content(id) ON DELETE CASCADE,
    -- Original file info (may no longer exist)
    original_path TEXT NOT NULL,
    original_size_bytes BIGINT,
    -- Which segments this source contributed to
    contributed_segments UUID[],
    -- Quality assessment when this source was analyzed
    quality_score REAL,
    -- When this source was incorporated
    contributed_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW()
);

-- Known patterns database for recognition (commercials, intros, outros, etc.)
CREATE TABLE known_patterns (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    -- 'commercial', 'intro', 'outro', 'recap', 'preview'
    pattern_type TEXT NOT NULL,
    -- Optional human-readable name (e.g., "Toyota Camry 2024" or "Breaking Bad Intro")
    name TEXT,
    -- For series-specific patterns (intros, outros, recaps)
    series_title TEXT,
    duration_ms INTEGER NOT NULL,
    -- Dense fingerprint for the pattern
    fingerprint_timeline BYTEA NOT NULL,
    fingerprint_interval_ms INTEGER NOT NULL,
    -- How many times this pattern has been detected
    detection_count INTEGER NOT NULL DEFAULT 1,
    first_seen_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    last_seen_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW()
);

-- Track where known patterns have been detected
CREATE TABLE pattern_detections (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    pattern_id UUID NOT NULL REFERENCES known_patterns(id) ON DELETE CASCADE,
    -- Either in a file or in canonical content
    file_id UUID REFERENCES files(id) ON DELETE CASCADE,
    content_id UUID REFERENCES canonical_content(id) ON DELETE CASCADE,
    start_ms INTEGER NOT NULL,
    end_ms INTEGER NOT NULL,
    confidence REAL NOT NULL,
    detected_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    CHECK (file_id IS NOT NULL OR content_id IS NOT NULL)
);

CREATE INDEX idx_canonical_content_dvr_id ON canonical_content(dvr_id);
CREATE INDEX idx_canonical_content_title ON canonical_content(title);
CREATE INDEX idx_canonical_segments_content_id ON canonical_segments(content_id);
CREATE INDEX idx_canonical_segments_type ON canonical_segments(segment_type);
CREATE INDEX idx_canonical_sources_content_id ON canonical_sources(content_id);
CREATE INDEX idx_known_patterns_type ON known_patterns(pattern_type);
CREATE INDEX idx_known_patterns_series ON known_patterns(series_title);
CREATE INDEX idx_known_patterns_duration ON known_patterns(duration_ms);
CREATE INDEX idx_pattern_detections_pattern_id ON pattern_detections(pattern_id);
CREATE INDEX idx_pattern_detections_file_id ON pattern_detections(file_id);
CREATE INDEX idx_pattern_detections_content_id ON pattern_detections(content_id);
