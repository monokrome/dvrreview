use chrono::{DateTime, Utc};
use diesel::deserialize::{self, FromSql};
use diesel::pg::{Pg, PgValue};
use diesel::prelude::*;
use diesel::serialize::{self, Output, ToSql};
use diesel::sql_types::Text;
use diesel::{AsExpression, FromSqlRow};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, FromSqlRow, AsExpression)]
#[diesel(sql_type = Text)]
pub enum FileStatus {
    Pending,
    Kept,
    Deleted,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, FromSqlRow, AsExpression)]
#[diesel(sql_type = Text)]
pub enum ReviewDecision {
    ConfirmedDuplicates,
    NotDuplicates,
    NeedsMoreReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, FromSqlRow, AsExpression)]
#[diesel(sql_type = Text)]
pub enum TranscodeStatus {
    Pending,
    Transcoding,
    Verifying,
    Completed,
    Failed,
}

impl ToSql<Text, Pg> for FileStatus {
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Pg>) -> serialize::Result {
        let s = match self {
            FileStatus::Pending => "pending",
            FileStatus::Kept => "kept",
            FileStatus::Deleted => "deleted",
            FileStatus::Skipped => "skipped",
        };
        <str as ToSql<Text, Pg>>::to_sql(s, out)
    }
}

impl FromSql<Text, Pg> for FileStatus {
    fn from_sql(bytes: PgValue<'_>) -> deserialize::Result<Self> {
        let s = <String as FromSql<Text, Pg>>::from_sql(bytes)?;
        match s.as_str() {
            "pending" => Ok(FileStatus::Pending),
            "kept" => Ok(FileStatus::Kept),
            "deleted" => Ok(FileStatus::Deleted),
            "skipped" => Ok(FileStatus::Skipped),
            _ => Err(format!("Unknown file_status: {}", s).into()),
        }
    }
}

impl ToSql<Text, Pg> for ReviewDecision {
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Pg>) -> serialize::Result {
        let s = match self {
            ReviewDecision::ConfirmedDuplicates => "confirmed_duplicates",
            ReviewDecision::NotDuplicates => "not_duplicates",
            ReviewDecision::NeedsMoreReview => "needs_more_review",
        };
        <str as ToSql<Text, Pg>>::to_sql(s, out)
    }
}

impl FromSql<Text, Pg> for ReviewDecision {
    fn from_sql(bytes: PgValue<'_>) -> deserialize::Result<Self> {
        let s = <String as FromSql<Text, Pg>>::from_sql(bytes)?;
        match s.as_str() {
            "confirmed_duplicates" => Ok(ReviewDecision::ConfirmedDuplicates),
            "not_duplicates" => Ok(ReviewDecision::NotDuplicates),
            "needs_more_review" => Ok(ReviewDecision::NeedsMoreReview),
            _ => Err(format!("Unknown review_decision: {}", s).into()),
        }
    }
}

impl ToSql<Text, Pg> for TranscodeStatus {
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Pg>) -> serialize::Result {
        let s = match self {
            TranscodeStatus::Pending => "pending",
            TranscodeStatus::Transcoding => "transcoding",
            TranscodeStatus::Verifying => "verifying",
            TranscodeStatus::Completed => "completed",
            TranscodeStatus::Failed => "failed",
        };
        <str as ToSql<Text, Pg>>::to_sql(s, out)
    }
}

impl FromSql<Text, Pg> for TranscodeStatus {
    fn from_sql(bytes: PgValue<'_>) -> deserialize::Result<Self> {
        let s = <String as FromSql<Text, Pg>>::from_sql(bytes)?;
        match s.as_str() {
            "pending" => Ok(TranscodeStatus::Pending),
            "transcoding" => Ok(TranscodeStatus::Transcoding),
            "verifying" => Ok(TranscodeStatus::Verifying),
            "completed" => Ok(TranscodeStatus::Completed),
            "failed" => Ok(TranscodeStatus::Failed),
            _ => Err(format!("Unknown transcode_status: {}", s).into()),
        }
    }
}

#[derive(Debug, Clone, Queryable, Selectable, Identifiable, Serialize)]
#[diesel(table_name = crate::db::schema::dvrs)]
pub struct Dvr {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_verified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = crate::db::schema::dvrs)]
pub struct NewDvr {
    pub name: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Identifiable, Associations, Serialize)]
#[diesel(table_name = crate::db::schema::files)]
#[diesel(belongs_to(Dvr))]
pub struct File {
    pub id: Uuid,
    pub path: String,
    pub size_bytes: i64,
    pub duration_ms: Option<i32>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub bitrate: Option<i32>,
    pub claimed_title: Option<String>,
    pub claimed_season: Option<i32>,
    pub claimed_episode: Option<i32>,
    pub content_start_ms: Option<i32>,
    pub content_end_ms: Option<i32>,
    pub status: FileStatus,
    pub created_at: DateTime<Utc>,
    pub fingerprinted_at: Option<DateTime<Utc>>,
    pub transcoded_path: Option<String>,
    pub transcoded_at: Option<DateTime<Utc>>,
    pub original_size_bytes: Option<i64>,
    pub transcode_status: TranscodeStatus,
    pub dvr_id: Option<Uuid>,
    pub relative_path: Option<String>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = crate::db::schema::files)]
pub struct NewFile {
    pub path: String,
    pub size_bytes: i64,
    pub duration_ms: Option<i32>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub bitrate: Option<i32>,
    pub claimed_title: Option<String>,
    pub claimed_season: Option<i32>,
    pub claimed_episode: Option<i32>,
    pub dvr_id: Option<Uuid>,
    pub relative_path: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Identifiable, Serialize)]
#[diesel(table_name = crate::db::schema::clusters)]
pub struct Cluster {
    pub id: Uuid,
    pub name: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = crate::db::schema::clusters)]
pub struct NewCluster {
    pub name: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Identifiable, Associations, Serialize)]
#[diesel(table_name = crate::db::schema::cluster_members)]
#[diesel(belongs_to(Cluster))]
#[diesel(belongs_to(File))]
#[diesel(primary_key(cluster_id, file_id))]
pub struct ClusterMember {
    pub cluster_id: Uuid,
    pub file_id: Uuid,
    pub similarity_score: Option<f32>,
    pub is_canonical: bool,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = crate::db::schema::cluster_members)]
pub struct NewClusterMember {
    pub cluster_id: Uuid,
    pub file_id: Uuid,
    pub similarity_score: Option<f32>,
}

#[derive(Debug, Clone, Queryable, Selectable, Identifiable, Associations, Serialize)]
#[diesel(table_name = crate::db::schema::fingerprints)]
#[diesel(belongs_to(File))]
pub struct Fingerprint {
    pub id: Uuid,
    pub file_id: Uuid,
    pub timestamp_ms: i32,
    pub frame_hash: Option<Vec<u8>>,
    pub audio_hash: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = crate::db::schema::fingerprints)]
pub struct NewFingerprint {
    pub file_id: Uuid,
    pub timestamp_ms: i32,
    pub frame_hash: Option<Vec<u8>>,
    pub audio_hash: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Identifiable, Associations, Serialize)]
#[diesel(table_name = crate::db::schema::thumbnails)]
#[diesel(belongs_to(File))]
pub struct Thumbnail {
    pub id: Uuid,
    pub file_id: Uuid,
    pub timestamp_ms: i32,
    pub path: String,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = crate::db::schema::thumbnails)]
pub struct NewThumbnail {
    pub file_id: Uuid,
    pub timestamp_ms: i32,
    pub path: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Identifiable, Associations, Serialize)]
#[diesel(table_name = crate::db::schema::reviews)]
#[diesel(belongs_to(Cluster))]
pub struct Review {
    pub id: Uuid,
    pub cluster_id: Uuid,
    pub decision: ReviewDecision,
    pub kept_file_id: Option<Uuid>,
    pub notes: Option<String>,
    pub reviewed_at: DateTime<Utc>,
    pub review_context: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = crate::db::schema::reviews)]
pub struct NewReview {
    pub cluster_id: Uuid,
    pub decision: ReviewDecision,
    pub kept_file_id: Option<Uuid>,
    pub notes: Option<String>,
    pub review_context: Option<serde_json::Value>,
}
