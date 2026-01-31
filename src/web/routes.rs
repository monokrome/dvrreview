use crate::db::models::{Cluster, ClusterMember, File, FileStatus, NewReview, NewThumbnail, ReviewDecision, Thumbnail};
use crate::db::schema::{cluster_members, clusters, files, reviews, thumbnails};
use crate::scanner::fingerprint::generate_thumbnail_timestamps;
use crate::thumbnail::extract_thumbnail;
use crate::web::server::AppState;
use askama::Template;
use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(index))
        .route("/review", get(review_page))
        .route("/review/{cluster_id}", get(review_cluster))
        .route("/review/{cluster_id}/decide", post(submit_decision))
        .route("/review/{cluster_id}/skip", post(skip_cluster))
        .route("/file/{file_id}/set-bounds", post(set_content_bounds))
        .route("/thumbnail/{file_id}/{timestamp_ms}", get(serve_thumbnail))
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate {
    pending_clusters: i64,
    total_files: i64,
    reviewed_count: i64,
}

async fn index(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut conn = match state.pool.get().await {
        Ok(c) => c,
        Err(e) => return Html(format!("Database error: {}", e)).into_response(),
    };

    let pending_clusters: i64 = clusters::table
        .left_join(reviews::table)
        .filter(reviews::id.is_null())
        .count()
        .get_result(&mut conn)
        .await
        .unwrap_or(0);

    let total_files: i64 = files::table
        .count()
        .get_result(&mut conn)
        .await
        .unwrap_or(0);

    let reviewed_count: i64 = reviews::table
        .count()
        .get_result(&mut conn)
        .await
        .unwrap_or(0);

    let template = IndexTemplate {
        pending_clusters,
        total_files,
        reviewed_count,
    };

    Html(template.render().unwrap_or_default()).into_response()
}

async fn review_page(State(state): State<Arc<AppState>>) -> Response {
    let mut conn = match state.pool.get().await {
        Ok(c) => c,
        Err(e) => return Html(format!("Database error: {}", e)).into_response(),
    };

    // Find unreviewed cluster IDs that have at least one transcoded file
    let ready_cluster_ids: Vec<Uuid> = cluster_members::table
        .inner_join(files::table)
        .filter(files::transcode_status.eq(crate::db::models::TranscodeStatus::Completed))
        .select(cluster_members::cluster_id)
        .distinct()
        .load(&mut conn)
        .await
        .unwrap_or_default();

    let cluster: Option<Cluster> = clusters::table
        .left_join(reviews::table)
        .filter(reviews::id.is_null())
        .filter(clusters::id.eq_any(&ready_cluster_ids))
        .select(Cluster::as_select())
        .first(&mut conn)
        .await
        .ok();

    match cluster {
        Some(c) => Redirect::to(&format!("/review/{}", c.id)).into_response(),
        None => Html("<h1>No clusters to review</h1><p><a href=\"/\">Back to home</a></p>")
            .into_response(),
    }
}

#[derive(Template)]
#[template(path = "review.html")]
struct ReviewTemplate {
    cluster: Cluster,
    files: Vec<FileWithThumbnails>,
}

struct FileWithThumbnails {
    file: File,
    thumbnails: Vec<ThumbnailDisplay>,
    relative_path: String,
    filename: String,
    resolution: String,
    bitrate_kbps: String,
    size_mb: i64,
    duration_display: String,
}

struct ThumbnailDisplay {
    file_id: Uuid,
    timestamp_ms: i32,
    timestamp_secs: i32,
}

async fn review_cluster(
    State(state): State<Arc<AppState>>,
    Path(cluster_id): Path<Uuid>,
) -> impl IntoResponse {
    let mut conn = match state.pool.get().await {
        Ok(c) => c,
        Err(e) => return Html(format!("Database error: {}", e)).into_response(),
    };

    let cluster: Cluster = match clusters::table
        .find(cluster_id)
        .first(&mut conn)
        .await
    {
        Ok(c) => c,
        Err(_) => return Html("Cluster not found").into_response(),
    };

    let members: Vec<ClusterMember> = cluster_members::table
        .filter(cluster_members::cluster_id.eq(cluster_id))
        .load(&mut conn)
        .await
        .unwrap_or_default();

    let file_ids: Vec<Uuid> = members.iter().map(|m| m.file_id).collect();

    let cluster_files: Vec<File> = files::table
        .filter(files::id.eq_any(&file_ids))
        .filter(files::transcode_status.eq(crate::db::models::TranscodeStatus::Completed))
        .load(&mut conn)
        .await
        .unwrap_or_default();

    let mut files_with_thumbs = Vec::new();
    for file in cluster_files {
        let thumb_displays: Vec<ThumbnailDisplay> = file
            .duration_ms
            .map(|d| {
                generate_thumbnail_timestamps(d, 8)
                    .into_iter()
                    .map(|ts| ThumbnailDisplay {
                        file_id: file.id,
                        timestamp_ms: ts,
                        timestamp_secs: ts / 1000,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let relative_path = file
            .path
            .strip_prefix(state.media_root.to_str().unwrap_or(""))
            .unwrap_or(&file.path)
            .trim_start_matches('/')
            .to_string();

        let filename = file
            .path
            .rsplit('/')
            .next()
            .unwrap_or(&file.path)
            .to_string();

        let resolution = match (file.width, file.height) {
            (Some(w), Some(h)) => format!("{}x{}", w, h),
            _ => String::new(),
        };

        let bitrate_kbps = file
            .bitrate
            .map(|b| format!("{} kbps", b / 1000))
            .unwrap_or_default();

        let size_mb = file.size_bytes / 1_000_000;

        let duration_display = file
            .duration_ms
            .map(|d| format!("{}:{:02}", d / 60000, (d % 60000) / 1000))
            .unwrap_or_default();

        files_with_thumbs.push(FileWithThumbnails {
            file,
            thumbnails: thumb_displays,
            relative_path,
            filename,
            resolution,
            bitrate_kbps,
            size_mb,
            duration_display,
        });
    }

    // Sort by quality (resolution * bitrate descending)
    files_with_thumbs.sort_by(|a, b| {
        let quality_a =
            (a.file.width.unwrap_or(0) * a.file.height.unwrap_or(0)) as i64 * a.file.bitrate.unwrap_or(0) as i64;
        let quality_b =
            (b.file.width.unwrap_or(0) * b.file.height.unwrap_or(0)) as i64 * b.file.bitrate.unwrap_or(0) as i64;
        quality_b.cmp(&quality_a)
    });

    if files_with_thumbs.is_empty() {
        return Html(format!(
            "<h1>Cluster not ready</h1><p>No transcoded files available for cluster \"{}\". \
             Transcode files first, then review.</p><p><a href=\"/\">Back to home</a></p>",
            cluster.name.as_deref().unwrap_or("Unnamed")
        ))
        .into_response();
    }

    let template = ReviewTemplate {
        cluster,
        files: files_with_thumbs,
    };

    Html(template.render().unwrap_or_default()).into_response()
}

#[derive(Deserialize)]
struct DecisionForm {
    kept_file_id: Uuid,
    notes: Option<String>,
}

async fn submit_decision(
    State(state): State<Arc<AppState>>,
    Path(cluster_id): Path<Uuid>,
    Form(form): Form<DecisionForm>,
) -> Response {
    let mut conn = match state.pool.get().await {
        Ok(c) => c,
        Err(e) => return Html(format!("Database error: {}", e)).into_response(),
    };

    // Mark all files in cluster as deleted except the kept one
    let members: Vec<ClusterMember> = cluster_members::table
        .filter(cluster_members::cluster_id.eq(cluster_id))
        .load(&mut conn)
        .await
        .unwrap_or_default();

    for member in &members {
        let new_status = if member.file_id == form.kept_file_id {
            FileStatus::Kept
        } else {
            FileStatus::Deleted
        };

        let _ = diesel::update(files::table.find(member.file_id))
            .set(files::status.eq(new_status))
            .execute(&mut conn)
            .await;
    }

    // Mark the kept file as canonical
    let _ = diesel::update(cluster_members::table.find((cluster_id, form.kept_file_id)))
        .set(cluster_members::is_canonical.eq(true))
        .execute(&mut conn)
        .await;

    // Record the review
    let review = NewReview {
        cluster_id,
        decision: ReviewDecision::ConfirmedDuplicates,
        kept_file_id: Some(form.kept_file_id),
        notes: form.notes,
        review_context: Some(serde_json::json!({
            "file_ids": members.iter().map(|m| m.file_id.to_string()).collect::<Vec<_>>(),
        })),
    };

    let _ = diesel::insert_into(reviews::table)
        .values(&review)
        .execute(&mut conn)
        .await;

    Redirect::to("/review").into_response()
}

async fn skip_cluster(
    State(state): State<Arc<AppState>>,
    Path(cluster_id): Path<Uuid>,
) -> Response {
    let mut conn = match state.pool.get().await {
        Ok(c) => c,
        Err(e) => return Html(format!("Database error: {}", e)).into_response(),
    };

    // Mark files as skipped (will regenerate thumbnails on next pass)
    let members: Vec<ClusterMember> = cluster_members::table
        .filter(cluster_members::cluster_id.eq(cluster_id))
        .load(&mut conn)
        .await
        .unwrap_or_default();

    for member in &members {
        let _ = diesel::update(files::table.find(member.file_id))
            .set(files::status.eq(FileStatus::Skipped))
            .execute(&mut conn)
            .await;
    }

    // Record the skip as needs_more_review
    let review = NewReview {
        cluster_id,
        decision: ReviewDecision::NeedsMoreReview,
        kept_file_id: None,
        notes: Some("Skipped - regenerate thumbnails".to_string()),
        review_context: None,
    };

    let _ = diesel::insert_into(reviews::table)
        .values(&review)
        .execute(&mut conn)
        .await;

    Redirect::to("/review").into_response()
}

#[derive(Deserialize)]
struct ContentBoundsForm {
    content_start_ms: Option<i32>,
    content_end_ms: Option<i32>,
}

async fn set_content_bounds(
    State(state): State<Arc<AppState>>,
    Path(file_id): Path<Uuid>,
    Form(form): Form<ContentBoundsForm>,
) -> Response {
    let mut conn = match state.pool.get().await {
        Ok(c) => c,
        Err(e) => return Html(format!("Database error: {}", e)).into_response(),
    };

    let _ = diesel::update(files::table.find(file_id))
        .set((
            files::content_start_ms.eq(form.content_start_ms),
            files::content_end_ms.eq(form.content_end_ms),
        ))
        .execute(&mut conn)
        .await;

    Html("OK").into_response()
}

async fn serve_thumbnail(
    State(state): State<Arc<AppState>>,
    Path((file_id, timestamp_ms)): Path<(Uuid, i32)>,
) -> Response {
    let mut conn = match state.pool.get().await {
        Ok(c) => c,
        Err(e) => return Html(format!("Database error: {}", e)).into_response(),
    };

    // Check DB for existing thumbnail
    let existing: Option<Thumbnail> = thumbnails::table
        .filter(thumbnails::file_id.eq(file_id))
        .filter(thumbnails::timestamp_ms.eq(timestamp_ms))
        .first(&mut conn)
        .await
        .ok();

    if let Some(thumb) = existing {
        return (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::CACHE_CONTROL, "public, max-age=86400"),
            ],
            thumb.data,
        )
            .into_response();
    }

    // Not in DB — generate on the fly
    let file: File = match files::table.find(file_id).first(&mut conn).await {
        Ok(f) => f,
        Err(_) => return (axum::http::StatusCode::NOT_FOUND, "File not found").into_response(),
    };

    let video_path = match &file.relative_path {
        Some(rel) => state.media_root.join(rel),
        None => PathBuf::from(&file.path),
    };

    let data = match extract_thumbnail(&video_path, timestamp_ms).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("Thumbnail extraction failed for {}@{}ms: {}", file_id, timestamp_ms, e);
            return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "Thumbnail generation failed").into_response();
        }
    };

    if state.preserve_thumbnails {
        let new_thumb = NewThumbnail {
            file_id,
            timestamp_ms,
            data: data.clone(),
        };

        let _ = diesel::insert_into(thumbnails::table)
            .values(&new_thumb)
            .on_conflict((thumbnails::file_id, thumbnails::timestamp_ms))
            .do_nothing()
            .execute(&mut conn)
            .await;
    }

    (
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        data,
    )
        .into_response()
}
