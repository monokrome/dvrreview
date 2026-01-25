use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use dvrreview::db::models::{File, NewCluster, NewClusterMember, NewFile, NewFingerprint, NewThumbnail};
use dvrreview::db::schema::{cluster_members, clusters, files, fingerprints, thumbnails};
use dvrreview::db::{self, DbPool};
use dvrreview::scanner::fingerprint::{generate_sample_timestamps, generate_thumbnail_timestamps, Fingerprinter};
use dvrreview::scanner::metadata::{parse_filename, MediaMetadata};
use dvrreview::cluster::ClusterBuilder;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;
use walkdir::WalkDir;

#[derive(Parser)]
#[command(name = "dvrreview", about = "DVR duplicate detection and review tool")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan a directory for video files and extract metadata
    Scan {
        /// Directory to scan
        #[arg(short, long)]
        path: PathBuf,

        /// File extensions to include (default: ts,mpg,mpeg,mp4,mkv,avi)
        #[arg(short, long, default_value = "ts,mpg,mpeg,mp4,mkv,avi")]
        extensions: String,
    },

    /// Generate fingerprints for scanned files
    Fingerprint {
        /// Number of frames to sample per file
        #[arg(short, long, default_value = "10")]
        samples: usize,

        /// Only process files without fingerprints
        #[arg(long, default_value = "true")]
        incremental: bool,
    },

    /// Generate thumbnail images for the review UI
    Thumbnails {
        /// Output directory for thumbnails
        #[arg(short, long)]
        output: PathBuf,

        /// Number of thumbnails per file
        #[arg(short, long, default_value = "8")]
        count: usize,
    },

    /// Build clusters from fingerprints
    Cluster {
        /// Similarity threshold (0.0-1.0)
        #[arg(short, long, default_value = "0.85")]
        threshold: f32,
    },

    /// Start the review web server
    Serve {
        /// Address to bind to
        #[arg(short, long, default_value = "127.0.0.1:3000")]
        addr: SocketAddr,

        /// Directory containing thumbnails
        #[arg(short, long)]
        thumbnails: PathBuf,

        /// Root directory of media files (for serving video)
        #[arg(short, long)]
        media: PathBuf,
    },

    /// Show statistics
    Stats,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("dvrreview=info".parse()?))
        .init();

    let cli = Cli::parse();
    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL must be set")?;
    let pool = db::create_pool(&database_url);

    match cli.command {
        Command::Scan { path, extensions } => {
            scan_directory(&pool, &path, &extensions).await?;
        }
        Command::Fingerprint { samples, incremental } => {
            generate_fingerprints(&pool, samples, incremental).await?;
        }
        Command::Thumbnails { output, count } => {
            generate_thumbnails(&pool, &output, count).await?;
        }
        Command::Cluster { threshold } => {
            build_clusters(&pool, threshold).await?;
        }
        Command::Serve { addr, thumbnails, media } => {
            dvrreview::web::run_server(pool, thumbnails, media, addr).await?;
        }
        Command::Stats => {
            show_stats(&pool).await?;
        }
    }

    Ok(())
}

async fn scan_directory(pool: &DbPool, path: &PathBuf, extensions: &str) -> Result<()> {
    let exts: Vec<&str> = extensions.split(',').collect();

    tracing::info!("Scanning {:?} for files with extensions: {:?}", path, exts);

    let mut conn = pool.get().await?;
    let mut scanned = 0;
    let mut skipped = 0;

    for entry in WalkDir::new(path)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let file_path = entry.path();

        if !file_path.is_file() {
            continue;
        }

        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        if !exts.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
            continue;
        }

        let path_str = file_path.to_string_lossy().to_string();

        // Check if already in DB
        let exists: bool = diesel::select(diesel::dsl::exists(
            files::table.filter(files::path.eq(&path_str)),
        ))
        .get_result(&mut conn)
        .await?;

        if exists {
            skipped += 1;
            continue;
        }

        // Get file size
        let metadata = std::fs::metadata(file_path)?;
        let size_bytes = metadata.len() as i64;

        // Extract media metadata
        let media_meta = match MediaMetadata::extract(file_path) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("Failed to extract metadata from {:?}: {}", file_path, e);
                continue;
            }
        };

        // Parse filename for claimed show info
        let filename = file_path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("");
        let parsed = parse_filename(filename);

        let new_file = NewFile {
            path: path_str,
            size_bytes,
            duration_ms: media_meta.duration_ms,
            video_codec: media_meta.video_codec,
            audio_codec: media_meta.audio_codec,
            width: media_meta.width,
            height: media_meta.height,
            bitrate: media_meta.bitrate,
            claimed_title: parsed.title,
            claimed_season: parsed.season,
            claimed_episode: parsed.episode,
        };

        diesel::insert_into(files::table)
            .values(&new_file)
            .execute(&mut conn)
            .await?;

        scanned += 1;

        if scanned % 100 == 0 {
            tracing::info!("Scanned {} files...", scanned);
        }
    }

    tracing::info!(
        "Scan complete. Added {} new files, skipped {} existing.",
        scanned,
        skipped
    );

    Ok(())
}

async fn generate_fingerprints(pool: &DbPool, samples: usize, incremental: bool) -> Result<()> {
    let mut conn = pool.get().await?;

    let files_to_process: Vec<File> = if incremental {
        files::table
            .filter(files::fingerprinted_at.is_null())
            .filter(files::duration_ms.is_not_null())
            .load(&mut conn)
            .await?
    } else {
        files::table
            .filter(files::duration_ms.is_not_null())
            .load(&mut conn)
            .await?
    };

    tracing::info!("Generating fingerprints for {} files", files_to_process.len());

    let fingerprinter = Fingerprinter::new();

    for (i, file) in files_to_process.iter().enumerate() {
        let duration = match file.duration_ms {
            Some(d) => d,
            None => continue,
        };

        let timestamps = generate_sample_timestamps(duration, samples);
        let path = PathBuf::from(&file.path);

        match fingerprinter.extract_frame_hashes(&path, &timestamps) {
            Ok(hashes) => {
                for (ts, hash) in hashes {
                    let new_fp = NewFingerprint {
                        file_id: file.id,
                        timestamp_ms: ts,
                        frame_hash: Some(hash),
                        audio_hash: None,
                    };

                    diesel::insert_into(fingerprints::table)
                        .values(&new_fp)
                        .on_conflict((fingerprints::file_id, fingerprints::timestamp_ms))
                        .do_nothing()
                        .execute(&mut conn)
                        .await?;
                }

                diesel::update(files::table.find(file.id))
                    .set(files::fingerprinted_at.eq(chrono::Utc::now()))
                    .execute(&mut conn)
                    .await?;
            }
            Err(e) => {
                tracing::warn!("Failed to fingerprint {:?}: {}", file.path, e);
            }
        }

        if (i + 1) % 10 == 0 {
            tracing::info!("Fingerprinted {}/{} files", i + 1, files_to_process.len());
        }
    }

    tracing::info!("Fingerprinting complete.");
    Ok(())
}

async fn generate_thumbnails(pool: &DbPool, output_dir: &PathBuf, count: usize) -> Result<()> {
    std::fs::create_dir_all(output_dir)?;

    let mut conn = pool.get().await?;

    // Get files that don't have thumbnails yet
    let files_needing_thumbs: Vec<File> = files::table
        .left_join(thumbnails::table)
        .filter(thumbnails::id.is_null())
        .filter(files::duration_ms.is_not_null())
        .select(File::as_select())
        .load(&mut conn)
        .await?;

    tracing::info!("Generating thumbnails for {} files", files_needing_thumbs.len());

    for (i, file) in files_needing_thumbs.iter().enumerate() {
        let duration = match file.duration_ms {
            Some(d) => d,
            None => continue,
        };

        let timestamps = generate_thumbnail_timestamps(duration, count);

        for ts in timestamps {
            let ts_secs = ts as f64 / 1000.0;
            let thumb_filename = format!("{}_{}.jpg", file.id, ts);
            let thumb_path = output_dir.join(&thumb_filename);

            let status = std::process::Command::new("ffmpeg")
                .args([
                    "-ss",
                    &format!("{:.3}", ts_secs),
                    "-i",
                    &file.path,
                    "-vframes",
                    "1",
                    "-vf",
                    "scale=320:-1",
                    "-y",
                ])
                .arg(&thumb_path)
                .output();

            if let Ok(output) = status {
                if output.status.success() {
                    let new_thumb = NewThumbnail {
                        file_id: file.id,
                        timestamp_ms: ts,
                        path: thumb_path.to_string_lossy().to_string(),
                    };

                    diesel::insert_into(thumbnails::table)
                        .values(&new_thumb)
                        .on_conflict((thumbnails::file_id, thumbnails::timestamp_ms))
                        .do_nothing()
                        .execute(&mut conn)
                        .await?;
                }
            }
        }

        if (i + 1) % 10 == 0 {
            tracing::info!("Generated thumbnails for {}/{} files", i + 1, files_needing_thumbs.len());
        }
    }

    tracing::info!("Thumbnail generation complete.");
    Ok(())
}

async fn build_clusters(pool: &DbPool, threshold: f32) -> Result<()> {
    let mut conn = pool.get().await?;

    // Group files by claimed title first
    let all_files: Vec<File> = files::table
        .filter(files::fingerprinted_at.is_not_null())
        .load(&mut conn)
        .await?;

    let mut by_title: HashMap<String, Vec<File>> = HashMap::new();
    for file in all_files {
        let key = file.claimed_title.clone().unwrap_or_else(|| "Unknown".to_string());
        by_title.entry(key).or_default().push(file);
    }

    let builder = ClusterBuilder::new(threshold);
    let mut total_clusters = 0;

    for (title, title_files) in by_title {
        if title_files.len() < 2 {
            continue;
        }

        // Load fingerprints for these files
        let file_ids: Vec<Uuid> = title_files.iter().map(|f| f.id).collect();
        let fps: Vec<(Uuid, Vec<u8>)> = fingerprints::table
            .filter(fingerprints::file_id.eq_any(&file_ids))
            .filter(fingerprints::frame_hash.is_not_null())
            .select((fingerprints::file_id, fingerprints::frame_hash.assume_not_null()))
            .load(&mut conn)
            .await?;

        let mut fp_map: HashMap<Uuid, Vec<Vec<u8>>> = HashMap::new();
        for (file_id, hash) in fps {
            fp_map.entry(file_id).or_default().push(hash);
        }

        let matches = builder.find_similar_pairs(&title_files, &fp_map);
        let clusters = builder.build_clusters(&matches);

        for file_ids in clusters {
            let new_cluster = NewCluster {
                name: Some(format!("{} ({})", title, file_ids.len())),
            };

            let cluster: dvrreview::db::models::Cluster = diesel::insert_into(clusters::table)
                .values(&new_cluster)
                .get_result(&mut conn)
                .await?;

            for file_id in file_ids {
                let score = matches
                    .iter()
                    .find(|m| m.file_a == file_id || m.file_b == file_id)
                    .map(|m| m.score);

                let member = NewClusterMember {
                    cluster_id: cluster.id,
                    file_id,
                    similarity_score: score,
                };

                diesel::insert_into(cluster_members::table)
                    .values(&member)
                    .on_conflict((cluster_members::cluster_id, cluster_members::file_id))
                    .do_nothing()
                    .execute(&mut conn)
                    .await?;
            }

            total_clusters += 1;
        }
    }

    tracing::info!("Created {} clusters.", total_clusters);
    Ok(())
}

async fn show_stats(pool: &DbPool) -> Result<()> {
    let mut conn = pool.get().await?;

    let total_files: i64 = files::table.count().get_result(&mut conn).await?;

    let fingerprinted: i64 = files::table
        .filter(files::fingerprinted_at.is_not_null())
        .count()
        .get_result(&mut conn)
        .await?;

    let total_clusters: i64 = clusters::table.count().get_result(&mut conn).await?;

    let reviewed: i64 = dvrreview::db::schema::reviews::table
        .count()
        .get_result(&mut conn)
        .await?;

    println!("Total files:    {}", total_files);
    println!("Fingerprinted:  {}", fingerprinted);
    println!("Clusters:       {}", total_clusters);
    println!("Reviews:        {}", reviewed);

    Ok(())
}
