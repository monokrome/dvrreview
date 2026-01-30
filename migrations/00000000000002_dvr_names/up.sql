CREATE TABLE dvrs (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name TEXT NOT NULL UNIQUE,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    last_verified_at TIMESTAMP WITH TIME ZONE
);

ALTER TABLE files ADD COLUMN dvr_id UUID REFERENCES dvrs(id) ON DELETE CASCADE;
ALTER TABLE files ADD COLUMN relative_path TEXT;

CREATE INDEX idx_files_dvr_id ON files(dvr_id);
CREATE INDEX idx_dvrs_name ON dvrs(name);
