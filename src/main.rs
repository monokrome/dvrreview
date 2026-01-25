use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use dvrreview::db::models::{File, NewCluster, NewClusterMember, NewFile, NewFingerprint, NewThumbnail, TranscodeStatus};
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

    /// Transcode files to H.265/HEVC for space savings
    Transcode {
        /// CRF value (0-51, lower = better quality, default 23)
        #[arg(long, default_value = "23")]
        crf: u8,

        /// Encoding preset (ultrafast, fast, medium, slow, veryslow)
        #[arg(long, default_value = "medium")]
        preset: String,

        /// Use hardware encoding if available (NVENC, VAAPI, QSV)
        #[arg(long)]
        hardware: bool,

        /// Only transcode files with status 'kept'
        #[arg(long)]
        kept_only: bool,

        /// Maximum number of files to transcode (useful for testing)
        #[arg(long)]
        limit: Option<usize>,

        /// Skip verification (not recommended)
        #[arg(long)]
        no_verify: bool,
    },
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
        Command::Transcode {
            crf,
            preset,
            hardware,
            kept_only,
            limit,
            no_verify,
        } => {
            transcode_files(&pool, crf, preset, hardware, kept_only, limit, !no_verify).await?;
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

    // Transcode stats
    let transcoded: i64 = files::table
        .filter(files::transcode_status.eq(TranscodeStatus::Completed))
        .count()
        .get_result(&mut conn)
        .await?;

    let pending_transcode: i64 = files::table
        .filter(files::transcode_status.eq(TranscodeStatus::Pending))
        .count()
        .get_result(&mut conn)
        .await?;

    let failed_transcode: i64 = files::table
        .filter(files::transcode_status.eq(TranscodeStatus::Failed))
        .count()
        .get_result(&mut conn)
        .await?;

    // Calculate space savings
    let transcoded_files: Vec<File> = files::table
        .filter(files::transcode_status.eq(TranscodeStatus::Completed))
        .filter(files::original_size_bytes.is_not_null())
        .load(&mut conn)
        .await?;

    let savings: i64 = transcoded_files
        .iter()
        .map(|f| f.original_size_bytes.unwrap_or(0) - f.size_bytes)
        .sum();

    let all_files: Vec<File> = files::table.load(&mut conn).await?;
    let current_size: i64 = all_files.iter().map(|f| f.size_bytes).sum();

    println!("Files");
    println!("  Total:        {}", total_files);
    println!("  Fingerprinted:{}", fingerprinted);
    println!();
    println!("Dedup");
    println!("  Clusters:     {}", total_clusters);
    println!("  Reviews:      {}", reviewed);
    println!();
    println!("Transcode");
    println!("  Completed:    {}", transcoded);
    println!("  Pending:      {}", pending_transcode);
    println!("  Failed:       {}", failed_transcode);
    if savings > 0 {
        println!("  Space saved:  {:.2} GB", savings as f64 / 1_000_000_000.0);
    }
    println!();
    println!("Storage");
    println!("  Current size: {:.2} GB", current_size as f64 / 1_000_000_000.0);

    Ok(())
}

async fn transcode_files(
    pool: &DbPool,
    crf: u8,
    preset: String,
    use_hardware: bool,
    kept_only: bool,
    limit: Option<usize>,
    verify: bool,
) -> Result<()> {
    use dvrreview::transcode::{TranscodeConfig, Transcoder};

    let mut conn = pool.get().await?;

    // Clean up any stale .transcoding files and reset their status
    let stale_files: Vec<File> = files::table
        .filter(files::transcode_status.eq(TranscodeStatus::Transcoding))
        .load(&mut conn)
        .await?;

    for file in &stale_files {
        let transcoding_path = Transcoder::transcoding_path(&PathBuf::from(&file.path));
        if transcoding_path.exists() {
            tracing::info!("Cleaning up stale transcoding file: {:?}", transcoding_path);
            let _ = std::fs::remove_file(&transcoding_path);
        }

        diesel::update(files::table.find(file.id))
            .set(files::transcode_status.eq(TranscodeStatus::Pending))
            .execute(&mut conn)
            .await?;
    }

    // Get files to transcode, ordered by size descending (large first)
    let mut query = files::table
        .filter(files::transcode_status.eq(TranscodeStatus::Pending))
        .into_boxed();

    if kept_only {
        query = query.filter(files::status.eq(dvrreview::db::models::FileStatus::Kept));
    }

    let mut files_to_transcode: Vec<File> = query
        .order(files::size_bytes.desc())
        .load(&mut conn)
        .await?;

    if let Some(max) = limit {
        files_to_transcode.truncate(max);
    }

    if files_to_transcode.is_empty() {
        tracing::info!("No files to transcode.");
        return Ok(());
    }

    let total_size: i64 = files_to_transcode.iter().map(|f| f.size_bytes).sum();
    tracing::info!(
        "Transcoding {} files ({:.2} GB), largest first",
        files_to_transcode.len(),
        total_size as f64 / 1_000_000_000.0
    );

    if use_hardware {
        if let Some(hw) = dvrreview::transcode::detect_hardware_encoder() {
            tracing::info!("Using hardware encoder: {}", hw);
        } else {
            tracing::warn!("No hardware encoder detected, falling back to software");
        }
    }

    let config = TranscodeConfig {
        crf,
        preset,
        use_hardware,
        audio_codec: "aac".to_string(),
        container: "mkv".to_string(),
    };

    let transcoder = Transcoder::new(config);
    let mut total_saved: i64 = 0;
    let mut success_count = 0;
    let mut fail_count = 0;

    for (i, file) in files_to_transcode.iter().enumerate() {
        let input_path = PathBuf::from(&file.path);

        if !input_path.exists() {
            tracing::warn!("File not found, skipping: {:?}", input_path);
            diesel::update(files::table.find(file.id))
                .set(files::transcode_status.eq(TranscodeStatus::Failed))
                .execute(&mut conn)
                .await?;
            continue;
        }

        tracing::info!(
            "[{}/{}] Transcoding {:?} ({:.2} GB)",
            i + 1,
            files_to_transcode.len(),
            input_path.file_name().unwrap_or_default(),
            file.size_bytes as f64 / 1_000_000_000.0
        );

        // Mark as transcoding
        diesel::update(files::table.find(file.id))
            .set(files::transcode_status.eq(TranscodeStatus::Transcoding))
            .execute(&mut conn)
            .await?;

        // Transcode
        let result = match transcoder.transcode(&input_path, file.content_start_ms, file.content_end_ms) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Transcode failed: {}", e);
                transcoder.cleanup_failed(&Transcoder::transcoding_path(&input_path)).ok();
                diesel::update(files::table.find(file.id))
                    .set(files::transcode_status.eq(TranscodeStatus::Failed))
                    .execute(&mut conn)
                    .await?;
                fail_count += 1;
                continue;
            }
        };

        // Verify if enabled
        if verify {
            diesel::update(files::table.find(file.id))
                .set(files::transcode_status.eq(TranscodeStatus::Verifying))
                .execute(&mut conn)
                .await?;

            match transcoder.verify(&input_path, &result.transcoding_path) {
                Ok(v) if v.passed => {
                    tracing::info!("{}", v.message);
                }
                Ok(v) => {
                    tracing::error!("{}", v.message);
                    transcoder.cleanup_failed(&result.transcoding_path).ok();
                    diesel::update(files::table.find(file.id))
                        .set(files::transcode_status.eq(TranscodeStatus::Failed))
                        .execute(&mut conn)
                        .await?;
                    fail_count += 1;
                    continue;
                }
                Err(e) => {
                    tracing::error!("Verification error: {}", e);
                    transcoder.cleanup_failed(&result.transcoding_path).ok();
                    diesel::update(files::table.find(file.id))
                        .set(files::transcode_status.eq(TranscodeStatus::Failed))
                        .execute(&mut conn)
                        .await?;
                    fail_count += 1;
                    continue;
                }
            }
        }

        // Finalize - rename and delete original
        if let Err(e) = transcoder.finalize(&result, &input_path) {
            tracing::error!("Finalize failed: {}", e);
            diesel::update(files::table.find(file.id))
                .set(files::transcode_status.eq(TranscodeStatus::Failed))
                .execute(&mut conn)
                .await?;
            fail_count += 1;
            continue;
        }

        // Update database
        diesel::update(files::table.find(file.id))
            .set((
                files::transcode_status.eq(TranscodeStatus::Completed),
                files::transcoded_path.eq(result.final_path.to_string_lossy().to_string()),
                files::transcoded_at.eq(chrono::Utc::now()),
                files::original_size_bytes.eq(result.original_size),
                files::path.eq(result.final_path.to_string_lossy().to_string()),
                files::size_bytes.eq(result.transcoded_size),
            ))
            .execute(&mut conn)
            .await?;

        total_saved += result.savings_bytes();
        success_count += 1;

        tracing::info!(
            "Saved {:.2} GB ({:.1}%)",
            result.savings_bytes() as f64 / 1_000_000_000.0,
            result.savings_percent()
        );
    }

    tracing::info!(
        "Transcode complete. {} succeeded, {} failed. Total saved: {:.2} GB",
        success_count,
        fail_count,
        total_saved as f64 / 1_000_000_000.0
    );

    Ok(())
}
