// @generated automatically by Diesel CLI.

diesel::table! {
    cluster_members (cluster_id, file_id) {
        cluster_id -> Uuid,
        file_id -> Uuid,
        similarity_score -> Nullable<Float4>,
        is_canonical -> Bool,
    }
}

diesel::table! {
    clusters (id) {
        id -> Uuid,
        name -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    dvrs (id) {
        id -> Uuid,
        name -> Text,
        created_at -> Timestamptz,
        last_verified_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    files (id) {
        id -> Uuid,
        path -> Text,
        size_bytes -> Int8,
        duration_ms -> Nullable<Int4>,
        video_codec -> Nullable<Text>,
        audio_codec -> Nullable<Text>,
        width -> Nullable<Int4>,
        height -> Nullable<Int4>,
        bitrate -> Nullable<Int4>,
        claimed_title -> Nullable<Text>,
        claimed_season -> Nullable<Int4>,
        claimed_episode -> Nullable<Int4>,
        content_start_ms -> Nullable<Int4>,
        content_end_ms -> Nullable<Int4>,
        status -> Text,
        created_at -> Timestamptz,
        fingerprinted_at -> Nullable<Timestamptz>,
        transcoded_path -> Nullable<Text>,
        transcoded_at -> Nullable<Timestamptz>,
        original_size_bytes -> Nullable<Int8>,
        transcode_status -> Text,
        dvr_id -> Nullable<Uuid>,
        relative_path -> Nullable<Text>,
        tmdb_id -> Nullable<Int4>,
        tmdb_media_type -> Nullable<Text>,
        tmdb_title -> Nullable<Text>,
        tmdb_year -> Nullable<Int4>,
        identified_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    fingerprints (id) {
        id -> Uuid,
        file_id -> Uuid,
        timestamp_ms -> Int4,
        frame_hash -> Nullable<Bytea>,
        audio_hash -> Nullable<Bytea>,
    }
}

diesel::table! {
    reviews (id) {
        id -> Uuid,
        cluster_id -> Uuid,
        decision -> Text,
        kept_file_id -> Nullable<Uuid>,
        notes -> Nullable<Text>,
        reviewed_at -> Timestamptz,
        review_context -> Nullable<Jsonb>,
    }
}

diesel::table! {
    thumbnails (id) {
        id -> Uuid,
        file_id -> Uuid,
        timestamp_ms -> Int4,
        data -> Bytea,
    }
}

diesel::joinable!(cluster_members -> clusters (cluster_id));
diesel::joinable!(cluster_members -> files (file_id));
diesel::joinable!(files -> dvrs (dvr_id));
diesel::joinable!(fingerprints -> files (file_id));
diesel::joinable!(reviews -> clusters (cluster_id));
diesel::joinable!(reviews -> files (kept_file_id));
diesel::joinable!(thumbnails -> files (file_id));

diesel::allow_tables_to_appear_in_same_query!(
    cluster_members,
    clusters,
    dvrs,
    files,
    fingerprints,
    reviews,
    thumbnails,
);
