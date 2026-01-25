CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

CREATE TABLE files (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    path TEXT NOT NULL UNIQUE,
    size_bytes BIGINT NOT NULL,
    duration_ms INTEGER,
    video_codec TEXT,
    audio_codec TEXT,
    width INTEGER,
    height INTEGER,
    bitrate INTEGER,
    claimed_title TEXT,
    claimed_season INTEGER,
    claimed_episode INTEGER,
    content_start_ms INTEGER,
    content_end_ms INTEGER,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    fingerprinted_at TIMESTAMP WITH TIME ZONE
);

CREATE TABLE fingerprints (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    file_id UUID NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    timestamp_ms INTEGER NOT NULL,
    frame_hash BYTEA,
    audio_hash BYTEA,
    UNIQUE(file_id, timestamp_ms)
);

CREATE TABLE clusters (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name TEXT,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW()
);

CREATE TABLE cluster_members (
    cluster_id UUID NOT NULL REFERENCES clusters(id) ON DELETE CASCADE,
    file_id UUID NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    similarity_score REAL,
    is_canonical BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (cluster_id, file_id)
);

CREATE TABLE reviews (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    cluster_id UUID NOT NULL REFERENCES clusters(id) ON DELETE CASCADE,
    decision TEXT NOT NULL,
    kept_file_id UUID REFERENCES files(id) ON DELETE SET NULL,
    notes TEXT,
    reviewed_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    review_context JSONB
);

CREATE TABLE thumbnails (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    file_id UUID NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    timestamp_ms INTEGER NOT NULL,
    path TEXT NOT NULL,
    UNIQUE(file_id, timestamp_ms)
);

CREATE INDEX idx_files_claimed_title ON files(claimed_title);
CREATE INDEX idx_files_status ON files(status);
CREATE INDEX idx_fingerprints_file_id ON fingerprints(file_id);
CREATE INDEX idx_cluster_members_file_id ON cluster_members(file_id);
CREATE INDEX idx_thumbnails_file_id ON thumbnails(file_id);
