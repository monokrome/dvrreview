DROP INDEX IF EXISTS idx_files_transcode_status;
ALTER TABLE files DROP COLUMN IF EXISTS transcode_status;
ALTER TABLE files DROP COLUMN IF EXISTS original_size_bytes;
ALTER TABLE files DROP COLUMN IF EXISTS transcoded_at;
ALTER TABLE files DROP COLUMN IF EXISTS transcoded_path;
