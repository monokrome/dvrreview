ALTER TABLE files ADD COLUMN transcoded_path TEXT;
ALTER TABLE files ADD COLUMN transcoded_at TIMESTAMP WITH TIME ZONE;
ALTER TABLE files ADD COLUMN original_size_bytes BIGINT;
ALTER TABLE files ADD COLUMN transcode_status TEXT NOT NULL DEFAULT 'pending';

CREATE INDEX idx_files_transcode_status ON files(transcode_status);
