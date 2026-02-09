use anyhow::{Context, Result};
use cetane::prelude::*;
use postgres::Client;
use std::cell::RefCell;

use super::{normalize_sslmode, should_use_tls};

pub fn build_registry() -> MigrationRegistry {
    let mut registry = MigrationRegistry::new();

    registry.register(migration_0001_initial());
    registry.register(migration_0002_transcode());
    registry.register(migration_0003_dvr_names());
    registry.register(migration_0004_canonical_content());
    registry.register(migration_0005_thumbnail_data());
    registry.register(migration_0006_tmdb_identification());

    registry
}

fn migration_0001_initial() -> Migration {
    Migration::new("0001_initial")
        .operation(RunSql::new(
            "CREATE EXTENSION IF NOT EXISTS \"uuid-ossp\"",
        ).only_for(&["postgres"]))
        .operation(
            CreateTable::new("files")
                .add_field(
                    Field::new("id", FieldType::Uuid)
                        .primary_key()
                        .default("uuid_generate_v4()"),
                )
                .add_field(Field::new("path", FieldType::Text).not_null().unique())
                .add_field(Field::new("size_bytes", FieldType::BigInt).not_null())
                .add_field(Field::new("duration_ms", FieldType::Integer))
                .add_field(Field::new("video_codec", FieldType::Text))
                .add_field(Field::new("audio_codec", FieldType::Text))
                .add_field(Field::new("width", FieldType::Integer))
                .add_field(Field::new("height", FieldType::Integer))
                .add_field(Field::new("bitrate", FieldType::Integer))
                .add_field(Field::new("claimed_title", FieldType::Text))
                .add_field(Field::new("claimed_season", FieldType::Integer))
                .add_field(Field::new("claimed_episode", FieldType::Integer))
                .add_field(Field::new("content_start_ms", FieldType::Integer))
                .add_field(Field::new("content_end_ms", FieldType::Integer))
                .add_field(
                    Field::new("status", FieldType::Text)
                        .not_null()
                        .default("'pending'"),
                )
                .add_field(
                    Field::new("created_at", FieldType::TimestampTz)
                        .not_null()
                        .default("NOW()"),
                )
                .add_field(Field::new("fingerprinted_at", FieldType::TimestampTz)),
        )
        .operation(
            CreateTable::new("fingerprints")
                .add_field(
                    Field::new("id", FieldType::Uuid)
                        .primary_key()
                        .default("uuid_generate_v4()"),
                )
                .add_field(
                    Field::new("file_id", FieldType::Uuid)
                        .not_null()
                        .references("files", "id")
                        .on_delete(ReferentialAction::Cascade),
                )
                .add_field(Field::new("timestamp_ms", FieldType::Integer).not_null())
                .add_field(Field::new("frame_hash", FieldType::Binary))
                .add_field(Field::new("audio_hash", FieldType::Binary)),
        )
        .operation(AddConstraint::new(
            "fingerprints",
            Constraint::unique(
                "fingerprints_file_id_timestamp_ms_key",
                vec!["file_id".to_string(), "timestamp_ms".to_string()],
            ),
        ))
        .operation(
            CreateTable::new("clusters")
                .add_field(
                    Field::new("id", FieldType::Uuid)
                        .primary_key()
                        .default("uuid_generate_v4()"),
                )
                .add_field(Field::new("name", FieldType::Text))
                .add_field(
                    Field::new("created_at", FieldType::TimestampTz)
                        .not_null()
                        .default("NOW()"),
                ),
        )
        // cluster_members has a composite PK which cetane can't express natively
        .operation(RunSql::reversible(
            "CREATE TABLE cluster_members (\
                cluster_id UUID NOT NULL REFERENCES clusters(id) ON DELETE CASCADE, \
                file_id UUID NOT NULL REFERENCES files(id) ON DELETE CASCADE, \
                similarity_score REAL, \
                is_canonical BOOLEAN NOT NULL DEFAULT FALSE, \
                PRIMARY KEY (cluster_id, file_id)\
            )",
            "DROP TABLE cluster_members",
        ))
        .operation(
            CreateTable::new("reviews")
                .add_field(
                    Field::new("id", FieldType::Uuid)
                        .primary_key()
                        .default("uuid_generate_v4()"),
                )
                .add_field(
                    Field::new("cluster_id", FieldType::Uuid)
                        .not_null()
                        .references("clusters", "id")
                        .on_delete(ReferentialAction::Cascade),
                )
                .add_field(Field::new("decision", FieldType::Text).not_null())
                .add_field(
                    Field::new("kept_file_id", FieldType::Uuid)
                        .references("files", "id")
                        .on_delete(ReferentialAction::SetNull),
                )
                .add_field(Field::new("notes", FieldType::Text))
                .add_field(
                    Field::new("reviewed_at", FieldType::TimestampTz)
                        .not_null()
                        .default("NOW()"),
                )
                .add_field(Field::new("review_context", FieldType::JsonB)),
        )
        .operation(
            CreateTable::new("thumbnails")
                .add_field(
                    Field::new("id", FieldType::Uuid)
                        .primary_key()
                        .default("uuid_generate_v4()"),
                )
                .add_field(
                    Field::new("file_id", FieldType::Uuid)
                        .not_null()
                        .references("files", "id")
                        .on_delete(ReferentialAction::Cascade),
                )
                .add_field(Field::new("timestamp_ms", FieldType::Integer).not_null())
                .add_field(Field::new("path", FieldType::Text).not_null()),
        )
        .operation(AddConstraint::new(
            "thumbnails",
            Constraint::unique(
                "thumbnails_file_id_timestamp_ms_key",
                vec!["file_id".to_string(), "timestamp_ms".to_string()],
            ),
        ))
        .operation(AddIndex::new(
            "files",
            Index::new("idx_files_claimed_title").column("claimed_title"),
        ))
        .operation(AddIndex::new(
            "files",
            Index::new("idx_files_status").column("status"),
        ))
        .operation(AddIndex::new(
            "fingerprints",
            Index::new("idx_fingerprints_file_id").column("file_id"),
        ))
        .operation(AddIndex::new(
            "cluster_members",
            Index::new("idx_cluster_members_file_id").column("file_id"),
        ))
        .operation(AddIndex::new(
            "thumbnails",
            Index::new("idx_thumbnails_file_id").column("file_id"),
        ))
}

fn migration_0002_transcode() -> Migration {
    Migration::new("0002_transcode")
        .depends_on(&["0001_initial"])
        .operation(AddField::new(
            "files",
            Field::new("transcoded_path", FieldType::Text),
        ))
        .operation(AddField::new(
            "files",
            Field::new("transcoded_at", FieldType::TimestampTz),
        ))
        .operation(AddField::new(
            "files",
            Field::new("original_size_bytes", FieldType::BigInt),
        ))
        .operation(AddField::new(
            "files",
            Field::new("transcode_status", FieldType::Text)
                .not_null()
                .default("'pending'"),
        ))
        .operation(AddIndex::new(
            "files",
            Index::new("idx_files_transcode_status").column("transcode_status"),
        ))
}

fn migration_0003_dvr_names() -> Migration {
    Migration::new("0003_dvr_names")
        .depends_on(&["0001_initial"])
        .operation(
            CreateTable::new("dvrs")
                .add_field(
                    Field::new("id", FieldType::Uuid)
                        .primary_key()
                        .default("uuid_generate_v4()"),
                )
                .add_field(Field::new("name", FieldType::Text).not_null().unique())
                .add_field(
                    Field::new("created_at", FieldType::TimestampTz)
                        .not_null()
                        .default("NOW()"),
                )
                .add_field(Field::new("last_verified_at", FieldType::TimestampTz)),
        )
        .operation(AddField::new(
            "files",
            Field::new("dvr_id", FieldType::Uuid)
                .references("dvrs", "id")
                .on_delete(ReferentialAction::Cascade),
        ))
        .operation(AddField::new(
            "files",
            Field::new("relative_path", FieldType::Text),
        ))
        .operation(AddIndex::new(
            "files",
            Index::new("idx_files_dvr_id").column("dvr_id"),
        ))
        .operation(AddIndex::new(
            "dvrs",
            Index::new("idx_dvrs_name").column("name"),
        ))
}

fn migration_0004_canonical_content() -> Migration {
    Migration::new("0004_canonical_content")
        .depends_on(&["0003_dvr_names"])
        .operation(
            CreateTable::new("canonical_content")
                .add_field(
                    Field::new("id", FieldType::Uuid)
                        .primary_key()
                        .default("uuid_generate_v4()"),
                )
                .add_field(
                    Field::new("dvr_id", FieldType::Uuid)
                        .not_null()
                        .references("dvrs", "id")
                        .on_delete(ReferentialAction::Cascade),
                )
                .add_field(Field::new("title", FieldType::Text).not_null())
                .add_field(Field::new("season", FieldType::Integer))
                .add_field(Field::new("episode", FieldType::Integer))
                .add_field(Field::new("duration_ms", FieldType::Integer).not_null())
                .add_field(Field::new("file_path", FieldType::Text))
                .add_field(Field::new("fingerprint_timeline", FieldType::Binary))
                .add_field(Field::new("fingerprint_interval_ms", FieldType::Integer))
                .add_field(
                    Field::new("created_at", FieldType::TimestampTz)
                        .not_null()
                        .default("NOW()"),
                )
                .add_field(
                    Field::new("updated_at", FieldType::TimestampTz)
                        .not_null()
                        .default("NOW()"),
                ),
        )
        .operation(
            CreateTable::new("canonical_segments")
                .add_field(
                    Field::new("id", FieldType::Uuid)
                        .primary_key()
                        .default("uuid_generate_v4()"),
                )
                .add_field(
                    Field::new("content_id", FieldType::Uuid)
                        .not_null()
                        .references("canonical_content", "id")
                        .on_delete(ReferentialAction::Cascade),
                )
                .add_field(Field::new("start_ms", FieldType::Integer).not_null())
                .add_field(Field::new("end_ms", FieldType::Integer).not_null())
                .add_field(Field::new("segment_type", FieldType::Text).not_null())
                .add_field(Field::new("confidence", FieldType::Real).not_null())
                .add_field(Field::new("metadata", FieldType::JsonB))
                .add_field(
                    Field::new("created_at", FieldType::TimestampTz)
                        .not_null()
                        .default("NOW()"),
                ),
        )
        // canonical_sources has UUID[] column which cetane can't express
        .operation(RunSql::reversible(
            "CREATE TABLE canonical_sources (\
                id UUID PRIMARY KEY DEFAULT uuid_generate_v4(), \
                content_id UUID NOT NULL REFERENCES canonical_content(id) ON DELETE CASCADE, \
                original_path TEXT NOT NULL, \
                original_size_bytes BIGINT, \
                contributed_segments UUID[], \
                quality_score REAL, \
                contributed_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW()\
            )",
            "DROP TABLE canonical_sources",
        ))
        .operation(
            CreateTable::new("known_patterns")
                .add_field(
                    Field::new("id", FieldType::Uuid)
                        .primary_key()
                        .default("uuid_generate_v4()"),
                )
                .add_field(Field::new("pattern_type", FieldType::Text).not_null())
                .add_field(Field::new("name", FieldType::Text))
                .add_field(Field::new("series_title", FieldType::Text))
                .add_field(Field::new("duration_ms", FieldType::Integer).not_null())
                .add_field(
                    Field::new("fingerprint_timeline", FieldType::Binary).not_null(),
                )
                .add_field(
                    Field::new("fingerprint_interval_ms", FieldType::Integer).not_null(),
                )
                .add_field(
                    Field::new("detection_count", FieldType::Integer)
                        .not_null()
                        .default("1"),
                )
                .add_field(
                    Field::new("first_seen_at", FieldType::TimestampTz)
                        .not_null()
                        .default("NOW()"),
                )
                .add_field(
                    Field::new("last_seen_at", FieldType::TimestampTz)
                        .not_null()
                        .default("NOW()"),
                ),
        )
        // pattern_detections has a CHECK constraint which is cleaner as RunSql
        .operation(RunSql::reversible(
            "CREATE TABLE pattern_detections (\
                id UUID PRIMARY KEY DEFAULT uuid_generate_v4(), \
                pattern_id UUID NOT NULL REFERENCES known_patterns(id) ON DELETE CASCADE, \
                file_id UUID REFERENCES files(id) ON DELETE CASCADE, \
                content_id UUID REFERENCES canonical_content(id) ON DELETE CASCADE, \
                start_ms INTEGER NOT NULL, \
                end_ms INTEGER NOT NULL, \
                confidence REAL NOT NULL, \
                detected_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(), \
                CHECK (file_id IS NOT NULL OR content_id IS NOT NULL)\
            )",
            "DROP TABLE pattern_detections",
        ))
        .operation(AddIndex::new(
            "canonical_content",
            Index::new("idx_canonical_content_dvr_id").column("dvr_id"),
        ))
        .operation(AddIndex::new(
            "canonical_content",
            Index::new("idx_canonical_content_title").column("title"),
        ))
        .operation(AddIndex::new(
            "canonical_segments",
            Index::new("idx_canonical_segments_content_id").column("content_id"),
        ))
        .operation(AddIndex::new(
            "canonical_segments",
            Index::new("idx_canonical_segments_type").column("segment_type"),
        ))
        .operation(AddIndex::new(
            "canonical_sources",
            Index::new("idx_canonical_sources_content_id").column("content_id"),
        ))
        .operation(AddIndex::new(
            "known_patterns",
            Index::new("idx_known_patterns_type").column("pattern_type"),
        ))
        .operation(AddIndex::new(
            "known_patterns",
            Index::new("idx_known_patterns_series").column("series_title"),
        ))
        .operation(AddIndex::new(
            "known_patterns",
            Index::new("idx_known_patterns_duration").column("duration_ms"),
        ))
        .operation(AddIndex::new(
            "pattern_detections",
            Index::new("idx_pattern_detections_pattern_id").column("pattern_id"),
        ))
        .operation(AddIndex::new(
            "pattern_detections",
            Index::new("idx_pattern_detections_file_id").column("file_id"),
        ))
        .operation(AddIndex::new(
            "pattern_detections",
            Index::new("idx_pattern_detections_content_id").column("content_id"),
        ))
}

fn migration_0005_thumbnail_data() -> Migration {
    Migration::new("0005_thumbnail_data")
        .depends_on(&["0001_initial"])
        .operation(RemoveField::new("thumbnails", "path").with_definition(
            Field::new("path", FieldType::Text).not_null(),
        ))
        .operation(AddField::new(
            "thumbnails",
            Field::new("data", FieldType::Binary).not_null(),
        ))
}

fn migration_0006_tmdb_identification() -> Migration {
    Migration::new("0006_tmdb_identification")
        .depends_on(&["0001_initial"])
        .operation(AddField::new(
            "files",
            Field::new("tmdb_id", FieldType::Integer),
        ))
        .operation(AddField::new(
            "files",
            Field::new("tmdb_media_type", FieldType::Text),
        ))
        .operation(AddField::new(
            "files",
            Field::new("tmdb_title", FieldType::Text),
        ))
        .operation(AddField::new(
            "files",
            Field::new("tmdb_year", FieldType::Integer),
        ))
        .operation(AddField::new(
            "files",
            Field::new("identified_at", FieldType::TimestampTz),
        ))
        .operation(AddIndex::new(
            "files",
            Index::new("idx_files_tmdb_id").column("tmdb_id"),
        ))
}

const LEGACY_MIGRATIONS: &[&str] = &[
    "0001_initial",
    "0002_transcode",
    "0003_dvr_names",
    "0004_canonical_content",
    "0005_thumbnail_data",
];

fn connect(database_url: &str) -> Result<Client> {
    let use_tls = should_use_tls(database_url);
    let url = normalize_sslmode(database_url, use_tls);
    if use_tls {
        let tls = super::make_tls_connector();
        Client::connect(&url, tls).context("Failed to connect to database (TLS)")
    } else {
        Client::connect(&url, postgres::NoTls)
            .context("Failed to connect to database")
    }
}

pub fn run_migrations(database_url: &str) -> Result<()> {
    let mut state_client = connect(database_url)
        .context("Failed to connect to database for migration state")?;
    let exec_client = connect(database_url)
        .context("Failed to connect to database for migration execution")?;
    let exec = RefCell::new(exec_client);

    let mut state = PostgresMigrationState::new(&mut state_client)
        .map_err(|e| anyhow::anyhow!("Failed to initialize migration state: {}", e))?;

    // Seed state for existing databases migrated from Diesel SQL files.
    // If the files table exists but no cetane migrations have been recorded,
    // mark the legacy SQL migrations as already applied.
    let already_applied = state
        .applied_migrations()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    if already_applied.is_empty() {
        let has_files_table: bool = exec
            .borrow_mut()
            .query_one(
                "SELECT EXISTS (
                    SELECT 1 FROM information_schema.tables
                    WHERE table_name = 'files'
                )",
                &[],
            )
            .map(|row| row.get(0))
            .unwrap_or(false);

        if has_files_table {
            tracing::info!("Existing database detected, seeding migration state for legacy SQL migrations");
            for name in LEGACY_MIGRATIONS {
                state
                    .mark_applied(name)
                    .map_err(|e| anyhow::anyhow!("Failed to seed migration state: {}", e))?;
            }
        }
    }

    let registry = build_registry();
    let mut migrator = Migrator::new(&registry, &Postgres, state);

    let applied = migrator
        .migrate_forward_with_transactions(
            &mut |sql| {
                exec.borrow_mut()
                    .execute(sql, &[])
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            },
            &mut || {
                exec.borrow_mut()
                    .execute("BEGIN", &[])
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            },
            &mut || {
                exec.borrow_mut()
                    .execute("COMMIT", &[])
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            },
            &mut || {
                exec.borrow_mut()
                    .execute("ROLLBACK", &[])
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            },
        )
        .map_err(|e| anyhow::anyhow!("Migration failed: {}", e))?;

    if applied.is_empty() {
        tracing::debug!("All migrations already applied");
    } else {
        for name in &applied {
            tracing::info!("Applied migration: {}", name);
        }
    }

    Ok(())
}
