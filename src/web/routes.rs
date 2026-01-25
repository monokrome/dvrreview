use crate::db::models::{Cluster, ClusterMember, File, FileStatus, NewReview, ReviewDecision, Thumbnail};
use crate::db::schema::{cluster_members, clusters, files, reviews, thumbnails};
use crate::web::server::AppState;
use askama::Template;
use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde::Deserialize;
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

    // Find first unreviewed cluster
    let cluster: Option<Cluster> = clusters::table
        .left_join(reviews::table)
        .filter(reviews::id.is_null())
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
    timestamp_ms: i32,
    timestamp_secs: i32,
    filename: String,
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
        .load(&mut conn)
        .await
        .unwrap_or_default();

    let mut files_with_thumbs = Vec::new();
    for file in cluster_files {
        let thumbs: Vec<Thumbnail> = thumbnails::table
            .filter(thumbnails::file_id.eq(file.id))
            .order(thumbnails::timestamp_ms.asc())
            .load(&mut conn)
            .await
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

        let thumb_displays: Vec<ThumbnailDisplay> = thumbs
            .iter()
            .map(|t| {
                let thumb_filename = t
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&t.path)
                    .to_string();
                ThumbnailDisplay {
                    timestamp_ms: t.timestamp_ms,
                    timestamp_secs: t.timestamp_ms / 1000,
                    filename: thumb_filename,
                }
            })
            .collect();

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
