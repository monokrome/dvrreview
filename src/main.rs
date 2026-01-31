use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{Parser, Subcommand};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use dvrreview::db::models::{Dvr, File, MediaType, NewCluster, NewClusterMember, NewDvr, NewFile, NewFingerprint, NewThumbnail, TranscodeStatus};
use dvrreview::db::schema::{cluster_members, clusters, dvrs, files, fingerprints, thumbnails};
use dvrreview::db::{self, DbPool};
use dvrreview::scanner::fingerprint::{generate_sample_timestamps, generate_thumbnail_timestamps, Fingerprinter};
use dvrreview::scanner::metadata::{parse_filename, MediaMetadata};
use dvrreview::cluster::ClusterBuilder;
use rand::prelude::IndexedRandom;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;
use walkdir::WalkDir;

#[derive(Parser)]
#[command(name = "dvrreview", about = "DVR duplicate detection and review tool")]
struct Cli {
    /// Name of the DVR (used to identify this collection in the database)
    name: String,

    /// Path to the DVR root directory on this machine
    path: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan the DVR directory for video files and extract metadata
    Scan {
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

        /// Skip DVR verification before fingerprinting
        #[arg(long)]
        no_verify: bool,
    },

    /// Generate thumbnail images and store in DB for the review UI
    Thumbnails {
        /// Number of thumbnails per file
        #[arg(short, long, default_value = "8")]
        count: usize,

        /// Delete all thumbnails for this DVR from the database
        #[arg(long)]
        clear: bool,

        /// Skip confirmation prompt when clearing
        #[arg(long)]
        force: bool,
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

        /// Persist on-the-fly thumbnails to the database
        #[arg(short = 'p', long)]
        preserve_thumbnails: bool,
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

        /// Directory for temporary transcoding files (reduces NAS I/O contention)
        #[arg(long)]
        temp_dir: Option<PathBuf>,
    },

    /// Look up files on TMDB to normalize titles for clustering
    Identify {
        /// Log matches without writing to DB
        #[arg(long)]
        dry_run: bool,

        /// Only process files without an existing identification
        #[arg(long, default_value = "true")]
        incremental: bool,
    },

    /// Verify this is the correct DVR by checking fingerprints
    Verify,
}

struct DvrContext {
    dvr: Dvr,
    base_path: PathBuf,
}

impl DvrContext {
    fn resolve_path(&self, relative_path: &str) -> PathBuf {
        self.base_path.join(relative_path)
    }

    fn make_relative(&self, absolute_path: &PathBuf) -> Result<String> {
        let rel = absolute_path
            .strip_prefix(&self.base_path)
            .context("Path is not within DVR base directory")?;
        Ok(rel.to_string_lossy().to_string())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("dvrreview=info".parse()?))
        .init();

    let cli = Cli::parse();
    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL must be set")?;

    db::migrations::run_migrations(&database_url)?;

    let pool = db::create_pool(&database_url);

    // Canonicalize the base path
    let base_path = cli.path.canonicalize().context("Invalid DVR path")?;

    if !base_path.is_dir() {
        bail!("DVR path must be a directory: {:?}", base_path);
    }

    // Get or create the DVR record
    // Skip verification for verify, stats commands, and when --no-verify is passed
    let skip_verify = matches!(cli.command, Command::Verify | Command::Stats | Command::Identify { .. })
        || matches!(cli.command, Command::Fingerprint { no_verify: true, .. })
        || matches!(cli.command, Command::Transcode { no_verify: true, .. });
    let ctx = get_or_create_dvr(&pool, &cli.name, &base_path, skip_verify).await?;

    match cli.command {
        Command::Scan { extensions } => {
            scan_directory(&pool, &ctx, &extensions).await?;
        }
        Command::Fingerprint { samples, incremental, .. } => {
            generate_fingerprints(&pool, &ctx, samples, incremental).await?;
        }
        Command::Thumbnails { count, clear, force } => {
            if clear {
                clear_thumbnails(&pool, &ctx, force).await?;
            } else {
                generate_thumbnails(&pool, &ctx, count).await?;
            }
        }
        Command::Cluster { threshold } => {
            build_clusters(&pool, &ctx, threshold).await?;
        }
        Command::Serve { addr, preserve_thumbnails } => {
            dvrreview::web::run_server(pool, ctx.base_path.clone(), addr, preserve_thumbnails).await?;
        }
        Command::Stats => {
            show_stats(&pool, &ctx).await?;
        }
        Command::Transcode {
            crf,
            preset,
            hardware,
            kept_only,
            limit,
            no_verify,
            temp_dir,
        } => {
            transcode_files(&pool, &ctx, crf, preset, hardware, kept_only, limit, !no_verify, temp_dir).await?;
        }
        Command::Identify { dry_run, incremental } => {
            identify_files(&pool, &ctx, dry_run, incremental).await?;
        }
        Command::Verify => {
            verify_dvr(&pool, &ctx).await?;
        }
    }

    Ok(())
}

async fn get_or_create_dvr(pool: &DbPool, name: &str, base_path: &PathBuf, skip_verify: bool) -> Result<DvrContext> {
    let mut conn = pool.get().await.context("Failed to get database connection")?;

    // Try to find existing DVR
    let existing: Option<Dvr> = dvrs::table
        .filter(dvrs::name.eq(name))
        .first(&mut conn)
        .await
        .optional()?;

    let dvr = if let Some(dvr) = existing {
        tracing::info!("Found existing DVR: {} (id: {})", dvr.name, dvr.id);

        // Verify by sampling fingerprints (unless skipped)
        if !skip_verify {
            let sample_result = sample_verify(pool, &dvr, base_path).await?;

            if !sample_result.is_empty() && !sample_result.verified {
                bail!(
                    "DVR verification failed. {} of {} sampled files had matching fingerprints.\n\
                     This may not be the correct location for DVR '{}'.\n\
                     Run 'dvrreview {} {:?} verify' for detailed results.",
                    sample_result.matched,
                    sample_result.total,
                    name,
                    name,
                    base_path
                );
            }

            // Update last_verified_at
            diesel::update(dvrs::table.find(dvr.id))
                .set(dvrs::last_verified_at.eq(Utc::now()))
                .execute(&mut conn)
                .await?;
        }

        dvr
    } else {
        tracing::info!("Creating new DVR: {}", name);

        let new_dvr = NewDvr {
            name: name.to_string(),
        };

        diesel::insert_into(dvrs::table)
            .values(&new_dvr)
            .get_result(&mut conn)
            .await?
    };

    Ok(DvrContext {
        dvr,
        base_path: base_path.clone(),
    })
}

struct VerifyResult {
    total: usize,
    matched: usize,
    verified: bool,
}

impl VerifyResult {
    fn is_empty(&self) -> bool {
        self.total == 0
    }
}

async fn sample_verify(pool: &DbPool, dvr: &Dvr, base_path: &PathBuf) -> Result<VerifyResult> {
    let mut conn = pool.get().await.context("Failed to get database connection")?;

    // Get files with fingerprints for this DVR
    let fingerprinted_files: Vec<File> = files::table
        .filter(files::dvr_id.eq(dvr.id))
        .filter(files::fingerprinted_at.is_not_null())
        .filter(files::relative_path.is_not_null())
        .load(&mut conn)
        .await?;

    if fingerprinted_files.is_empty() {
        return Ok(VerifyResult {
            total: 0,
            matched: 0,
            verified: true,
        });
    }

    // Sample up to 5 random files
    let sample_size = std::cmp::min(5, fingerprinted_files.len());
    let mut rng = rand::rng();
    let sample: Vec<&File> = fingerprinted_files
        .choose_multiple(&mut rng, sample_size)
        .collect();

    let fingerprinter = Fingerprinter::new();
    let mut matched = 0;

    for file in &sample {
        let rel_path = match &file.relative_path {
            Some(p) => p,
            None => continue,
        };

        let full_path = base_path.join(rel_path);

        if !full_path.exists() {
            tracing::debug!("Sample file not found: {:?}", full_path);
            continue;
        }

        // Get stored fingerprints
        let stored_fps: Vec<(i32, Vec<u8>)> = fingerprints::table
            .filter(fingerprints::file_id.eq(file.id))
            .filter(fingerprints::frame_hash.is_not_null())
            .select((fingerprints::timestamp_ms, fingerprints::frame_hash.assume_not_null()))
            .limit(3)
            .load(&mut conn)
            .await?;

        if stored_fps.is_empty() {
            continue;
        }

        // Extract current fingerprints at same timestamps
        let timestamps: Vec<i32> = stored_fps.iter().map(|(ts, _)| *ts).collect();

        let current_fps = match fingerprinter.extract_frame_hashes(&full_path, &timestamps) {
            Ok(fps) => fps,
            Err(e) => {
                tracing::debug!("Failed to extract fingerprints from {:?}: {}", full_path, e);
                continue;
            }
        };

        // Compare fingerprints
        let mut file_matched = true;
        for (ts, stored_hash) in &stored_fps {
            if let Some((_, current_hash)) = current_fps.iter().find(|(t, _)| t == ts) {
                let similarity = fingerprinter.compare_hashes(stored_hash, current_hash);
                if similarity < 0.9 {
                    file_matched = false;
                    break;
                }
            } else {
                file_matched = false;
                break;
            }
        }

        if file_matched {
            matched += 1;
        }
    }

    // Require at least 80% of samples to match
    let verified = matched as f32 / sample.len() as f32 >= 0.8;

    Ok(VerifyResult {
        total: sample.len(),
        matched,
        verified,
    })
}

async fn verify_dvr(pool: &DbPool, ctx: &DvrContext) -> Result<()> {
    let mut conn = pool.get().await.context("Failed to get database connection")?;

    let fingerprinted_files: Vec<File> = files::table
        .filter(files::dvr_id.eq(ctx.dvr.id))
        .filter(files::fingerprinted_at.is_not_null())
        .filter(files::relative_path.is_not_null())
        .load(&mut conn)
        .await?;

    if fingerprinted_files.is_empty() {
        println!("No fingerprinted files found for DVR '{}'.", ctx.dvr.name);
        return Ok(());
    }

    let sample_size = std::cmp::min(10, fingerprinted_files.len());
    let mut rng = rand::rng();
    let sample: Vec<&File> = fingerprinted_files
        .choose_multiple(&mut rng, sample_size)
        .collect();

    let fingerprinter = Fingerprinter::new();
    let mut matched = 0;
    let mut missing = 0;
    let mut mismatched = 0;

    for file in &sample {
        let rel_path = match &file.relative_path {
            Some(p) => p,
            None => continue,
        };

        let full_path = ctx.resolve_path(rel_path);

        if !full_path.exists() {
            println!("  MISSING: {}", rel_path);
            missing += 1;
            continue;
        }

        let stored_fps: Vec<(i32, Vec<u8>)> = fingerprints::table
            .filter(fingerprints::file_id.eq(file.id))
            .filter(fingerprints::frame_hash.is_not_null())
            .select((fingerprints::timestamp_ms, fingerprints::frame_hash.assume_not_null()))
            .limit(3)
            .load(&mut conn)
            .await?;

        if stored_fps.is_empty() {
            println!("  NO FINGERPRINTS: {} (file_id: {})", rel_path, file.id);
            continue;
        }

        let timestamps: Vec<i32> = stored_fps.iter().map(|(ts, _)| *ts).collect();

        let current_fps = match fingerprinter.extract_frame_hashes(&full_path, &timestamps) {
            Ok(fps) => fps,
            Err(e) => {
                println!("  ERROR: {} - {}", rel_path, e);
                mismatched += 1;
                continue;
            }
        };

        let mut file_matched = true;
        for (ts, stored_hash) in &stored_fps {
            if let Some((_, current_hash)) = current_fps.iter().find(|(t, _)| t == ts) {
                let similarity = fingerprinter.compare_hashes(stored_hash, current_hash);
                if similarity < 0.9 {
                    file_matched = false;
                    break;
                }
            } else {
                file_matched = false;
                break;
            }
        }

        if file_matched {
            println!("  OK: {}", rel_path);
            matched += 1;
        } else {
            println!("  MISMATCH: {}", rel_path);
            mismatched += 1;
        }
    }

    println!();
    println!("Verified {} files:", sample_size);
    println!("  Matched:    {}", matched);
    println!("  Missing:    {}", missing);
    println!("  Mismatched: {}", mismatched);

    if matched as f32 / sample_size as f32 >= 0.8 {
        println!();
        println!("Verification PASSED. This appears to be the correct DVR location.");
    } else {
        println!();
        println!("Verification FAILED. This may not be the correct location for DVR '{}'.", ctx.dvr.name);
    }

    Ok(())
}

async fn scan_directory(pool: &DbPool, ctx: &DvrContext, extensions: &str) -> Result<()> {
    let exts: Vec<&str> = extensions.split(',').collect();

    tracing::info!("Scanning {:?} for files with extensions: {:?}", ctx.base_path, exts);

    let mut conn = pool.get().await.context("Failed to get database connection")?;
    let mut scanned = 0;
    let mut skipped = 0;

    for entry in WalkDir::new(&ctx.base_path)
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

        let abs_path = file_path.canonicalize()?;
        let relative_path = ctx.make_relative(&abs_path)?;

        // Check if already in DB for this DVR
        let exists: bool = diesel::select(diesel::dsl::exists(
            files::table
                .filter(files::dvr_id.eq(ctx.dvr.id))
                .filter(files::relative_path.eq(&relative_path)),
        ))
        .get_result(&mut conn)
        .await?;

        if exists {
            skipped += 1;
            continue;
        }

        // Get file size
        let metadata = std::fs::metadata(&abs_path)?;
        let size_bytes = metadata.len() as i64;

        // Extract media metadata
        let media_meta = match MediaMetadata::extract(&abs_path) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("Failed to extract metadata from {:?}: {}", abs_path, e);
                continue;
            }
        };

        // Parse filename for claimed show info
        let filename = abs_path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("");
        let parsed = parse_filename(filename);

        // Store full path for legacy compatibility, but also store relative
        let new_file = NewFile {
            path: abs_path.to_string_lossy().to_string(),
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
            dvr_id: Some(ctx.dvr.id),
            relative_path: Some(relative_path),
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

async fn generate_fingerprints(pool: &DbPool, ctx: &DvrContext, samples: usize, incremental: bool) -> Result<()> {
    let mut conn = pool.get().await.context("Failed to get database connection")?;

    // Skip files currently being transcoded
    let files_to_process: Vec<File> = if incremental {
        files::table
            .filter(files::dvr_id.eq(ctx.dvr.id))
            .filter(files::fingerprinted_at.is_null())
            .filter(files::duration_ms.is_not_null())
            .filter(files::transcode_status.ne(TranscodeStatus::Transcoding))
            .filter(files::transcode_status.ne(TranscodeStatus::Verifying))
            .load(&mut conn)
            .await?
    } else {
        files::table
            .filter(files::dvr_id.eq(ctx.dvr.id))
            .filter(files::duration_ms.is_not_null())
            .filter(files::transcode_status.ne(TranscodeStatus::Transcoding))
            .filter(files::transcode_status.ne(TranscodeStatus::Verifying))
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

        // Resolve path using relative_path if available, else fall back to absolute
        let path = match &file.relative_path {
            Some(rel) => ctx.resolve_path(rel),
            None => PathBuf::from(&file.path),
        };

        match fingerprinter.extract_frame_hashes(&path, &timestamps) {
            Ok(hashes) => {
                if hashes.is_empty() {
                    tracing::warn!("No frames extracted from {:?}", path);
                    continue;
                }

                for (ts, hash) in &hashes {
                    let new_fp = NewFingerprint {
                        file_id: file.id,
                        timestamp_ms: *ts,
                        frame_hash: Some(hash.clone()),
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
                    .set(files::fingerprinted_at.eq(Utc::now()))
                    .execute(&mut conn)
                    .await?;
            }
            Err(e) => {
                tracing::warn!("Failed to fingerprint {:?}: {}", path, e);
            }
        }

        if (i + 1) % 10 == 0 {
            tracing::info!("Fingerprinted {}/{} files", i + 1, files_to_process.len());
        }
    }

    tracing::info!("Fingerprinting complete.");
    Ok(())
}

async fn clear_thumbnails(pool: &DbPool, ctx: &DvrContext, force: bool) -> Result<()> {
    if !force {
        eprint!("Delete all thumbnails for DVR '{}'? (y/N) ", ctx.dvr.name);
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    let mut conn = pool.get().await.context("Failed to get database connection")?;

    let file_ids: Vec<Uuid> = files::table
        .filter(files::dvr_id.eq(ctx.dvr.id))
        .select(files::id)
        .load(&mut conn)
        .await?;

    let deleted: usize = diesel::delete(
        thumbnails::table.filter(thumbnails::file_id.eq_any(&file_ids)),
    )
    .execute(&mut conn)
    .await?;

    tracing::info!("Deleted {} thumbnails.", deleted);
    Ok(())
}

async fn generate_thumbnails(pool: &DbPool, ctx: &DvrContext, count: usize) -> Result<()> {
    let mut conn = pool.get().await.context("Failed to get database connection")?;

    let files_needing_thumbs: Vec<File> = files::table
        .left_join(thumbnails::table)
        .filter(files::dvr_id.eq(ctx.dvr.id))
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

        let file_path = match &file.relative_path {
            Some(rel) => ctx.resolve_path(rel),
            None => PathBuf::from(&file.path),
        };

        for ts in timestamps {
            let data = match dvrreview::thumbnail::extract_thumbnail(&file_path, ts).await {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("Failed to extract thumbnail at {}ms from {:?}: {}", ts, file_path, e);
                    continue;
                }
            };

            let new_thumb = NewThumbnail {
                file_id: file.id,
                timestamp_ms: ts,
                data,
            };

            diesel::insert_into(thumbnails::table)
                .values(&new_thumb)
                .on_conflict((thumbnails::file_id, thumbnails::timestamp_ms))
                .do_nothing()
                .execute(&mut conn)
                .await?;
        }

        if (i + 1) % 10 == 0 {
            tracing::info!("Generated thumbnails for {}/{} files", i + 1, files_needing_thumbs.len());
        }
    }

    tracing::info!("Thumbnail generation complete.");
    Ok(())
}

async fn identify_files(pool: &DbPool, ctx: &DvrContext, dry_run: bool, incremental: bool) -> Result<()> {
    let api_key = std::env::var("TMDB_API_KEY")
        .or_else(|_| std::env::var("TMDB_ACCESS_TOKEN"))
        .context("TMDB_API_KEY or TMDB_ACCESS_TOKEN must be set")?;

    let mut client = dvrreview::tmdb::TmdbClient::new(api_key)?;
    let mut conn = pool.get().await.context("Failed to get database connection")?;

    let files_to_identify: Vec<File> = if incremental {
        files::table
            .filter(files::dvr_id.eq(ctx.dvr.id))
            .filter(files::identified_at.is_null())
            .filter(files::claimed_title.is_not_null())
            .load(&mut conn)
            .await?
    } else {
        files::table
            .filter(files::dvr_id.eq(ctx.dvr.id))
            .filter(files::claimed_title.is_not_null())
            .load(&mut conn)
            .await?
    };

    if files_to_identify.is_empty() {
        tracing::info!("No files to identify.");
        return Ok(());
    }

    // Group by claimed_title to deduplicate API calls
    let mut by_title: HashMap<String, Vec<&File>> = HashMap::new();
    for file in &files_to_identify {
        if let Some(ref title) = file.claimed_title {
            by_title.entry(title.clone()).or_default().push(file);
        }
    }

    tracing::info!(
        "Identifying {} files ({} unique titles)",
        files_to_identify.len(),
        by_title.len()
    );

    let mut identified = 0;
    let mut not_found = 0;

    for (claimed_title, title_files) in &by_title {
        let parsed = dvrreview::tmdb::parse_title(claimed_title);
        let has_season = title_files.iter().any(|f| f.claimed_season.is_some());

        let result = match client.search(&parsed.clean_title, parsed.year, has_season).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("TMDB lookup failed for {:?}: {}", claimed_title, e);
                continue;
            }
        };

        match result {
            Some(ref tmdb) => {
                let year_str = tmdb.year.map(|y| y.to_string()).unwrap_or_default();
                tracing::info!(
                    "{:?} -> {} ({}, {}, id={})",
                    claimed_title,
                    tmdb.title,
                    tmdb.media_type,
                    year_str,
                    tmdb.id
                );

                if !dry_run {
                    let media_type = match tmdb.media_type.as_str() {
                        "tv" => MediaType::Tv,
                        _ => MediaType::Movie,
                    };

                    for file in title_files {
                        diesel::update(files::table.find(file.id))
                            .set((
                                files::tmdb_id.eq(tmdb.id),
                                files::tmdb_media_type.eq(&media_type),
                                files::tmdb_title.eq(&tmdb.title),
                                files::tmdb_year.eq(tmdb.year),
                                files::identified_at.eq(Utc::now()),
                            ))
                            .execute(&mut conn)
                            .await?;
                    }
                }

                identified += title_files.len();
            }
            None => {
                tracing::info!("{:?} -> no match found", claimed_title);

                if !dry_run {
                    // Mark as identified (with null tmdb_id) so incremental skips them
                    for file in title_files {
                        diesel::update(files::table.find(file.id))
                            .set(files::identified_at.eq(Utc::now()))
                            .execute(&mut conn)
                            .await?;
                    }
                }

                not_found += title_files.len();
            }
        }
    }

    tracing::info!(
        "Identification complete. {} identified, {} not found.{}",
        identified,
        not_found,
        if dry_run { " (dry run)" } else { "" }
    );

    Ok(())
}

async fn build_clusters(pool: &DbPool, ctx: &DvrContext, threshold: f32) -> Result<()> {
    let mut conn = pool.get().await.context("Failed to get database connection")?;

    // Skip files currently being transcoded
    let all_files: Vec<File> = files::table
        .filter(files::dvr_id.eq(ctx.dvr.id))
        .filter(files::fingerprinted_at.is_not_null())
        .filter(files::transcode_status.ne(TranscodeStatus::Transcoding))
        .filter(files::transcode_status.ne(TranscodeStatus::Verifying))
        .load(&mut conn)
        .await?;

    let mut by_title: HashMap<String, Vec<File>> = HashMap::new();
    for file in all_files {
        let key = if let Some(tmdb_id) = file.tmdb_id {
            format!("tmdb:{}", tmdb_id)
        } else {
            format!("title:{}", file.claimed_title.as_deref().unwrap_or("Unknown"))
        };
        by_title.entry(key).or_default().push(file);
    }

    let builder = ClusterBuilder::new(threshold);
    let mut total_clusters = 0;

    for (title, title_files) in by_title {
        if title_files.len() < 2 {
            continue;
        }

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

async fn show_stats(pool: &DbPool, ctx: &DvrContext) -> Result<()> {
    let mut conn = pool.get().await.context("Failed to get database connection")?;

    let total_files: i64 = files::table
        .filter(files::dvr_id.eq(ctx.dvr.id))
        .count()
        .get_result(&mut conn)
        .await?;

    let fingerprinted: i64 = files::table
        .filter(files::dvr_id.eq(ctx.dvr.id))
        .filter(files::fingerprinted_at.is_not_null())
        .count()
        .get_result(&mut conn)
        .await?;

    // Count files that actually have fingerprint records
    let file_ids_for_dvr: Vec<Uuid> = files::table
        .filter(files::dvr_id.eq(ctx.dvr.id))
        .select(files::id)
        .load(&mut conn)
        .await?;

    let files_with_fps: i64 = fingerprints::table
        .filter(fingerprints::file_id.eq_any(&file_ids_for_dvr))
        .select(fingerprints::file_id)
        .distinct()
        .count()
        .get_result(&mut conn)
        .await?;

    let total_fp_records: i64 = fingerprints::table
        .filter(fingerprints::file_id.eq_any(&file_ids_for_dvr))
        .count()
        .get_result(&mut conn)
        .await?;

    let total_clusters: i64 = clusters::table.count().get_result(&mut conn).await?;

    let reviewed: i64 = dvrreview::db::schema::reviews::table
        .count()
        .get_result(&mut conn)
        .await?;

    let transcoded: i64 = files::table
        .filter(files::dvr_id.eq(ctx.dvr.id))
        .filter(files::transcode_status.eq(TranscodeStatus::Completed))
        .count()
        .get_result(&mut conn)
        .await?;

    let pending_transcode: i64 = files::table
        .filter(files::dvr_id.eq(ctx.dvr.id))
        .filter(files::transcode_status.eq(TranscodeStatus::Pending))
        .count()
        .get_result(&mut conn)
        .await?;

    let failed_transcode: i64 = files::table
        .filter(files::dvr_id.eq(ctx.dvr.id))
        .filter(files::transcode_status.eq(TranscodeStatus::Failed))
        .count()
        .get_result(&mut conn)
        .await?;

    let transcoded_files: Vec<File> = files::table
        .filter(files::dvr_id.eq(ctx.dvr.id))
        .filter(files::transcode_status.eq(TranscodeStatus::Completed))
        .filter(files::original_size_bytes.is_not_null())
        .load(&mut conn)
        .await?;

    let savings: i64 = transcoded_files
        .iter()
        .map(|f| f.original_size_bytes.unwrap_or(0) - f.size_bytes)
        .sum();

    let all_files: Vec<File> = files::table
        .filter(files::dvr_id.eq(ctx.dvr.id))
        .load(&mut conn)
        .await?;

    let current_size: i64 = all_files.iter().map(|f| f.size_bytes).sum();

    println!("DVR: {} (id: {})", ctx.dvr.name, ctx.dvr.id);
    println!("Path: {:?}", ctx.base_path);
    if let Some(verified) = ctx.dvr.last_verified_at {
        println!("Last verified: {}", verified);
    }
    println!();
    println!("Files");
    println!("  Total:           {}", total_files);
    println!("  Fingerprinted:   {} (flag set)", fingerprinted);
    println!("  With FP records: {} ({} records)", files_with_fps, total_fp_records);
    println!();
    println!("Dedup");
    println!("  Clusters:        {}", total_clusters);
    println!("  Reviews:         {}", reviewed);
    println!();
    println!("Transcode");
    println!("  Completed:       {}", transcoded);
    println!("  Pending:         {}", pending_transcode);
    println!("  Failed:          {}", failed_transcode);
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
    ctx: &DvrContext,
    crf: u8,
    preset: String,
    use_hardware: bool,
    kept_only: bool,
    limit: Option<usize>,
    verify: bool,
    temp_dir: Option<PathBuf>,
) -> Result<()> {
    use dvrreview::transcode::{TranscodeConfig, Transcoder};

    let mut conn = pool.get().await.context("Failed to get database connection")?;

    // Clean up any stale .transcoding files and reset their status
    let stale_files: Vec<File> = files::table
        .filter(files::dvr_id.eq(ctx.dvr.id))
        .filter(files::transcode_status.eq(TranscodeStatus::Transcoding))
        .load(&mut conn)
        .await?;

    for file in &stale_files {
        let file_path = match &file.relative_path {
            Some(rel) => ctx.resolve_path(rel),
            None => PathBuf::from(&file.path),
        };

        // Check both the default location (next to original) and temp dir
        for path in [
            Transcoder::transcoding_path(&file_path, None),
            Transcoder::transcoding_path(&file_path, temp_dir.as_deref()),
        ] {
            if path.exists() {
                tracing::info!("Cleaning up stale transcoding file: {:?}", path);
                let _ = std::fs::remove_file(&path);
            }
        }

        diesel::update(files::table.find(file.id))
            .set(files::transcode_status.eq(TranscodeStatus::Pending))
            .execute(&mut conn)
            .await?;
    }

    // Get files to transcode, ordered by size descending (large first)
    let mut query = files::table
        .filter(files::dvr_id.eq(ctx.dvr.id))
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

    if let Some(ref dir) = temp_dir {
        std::fs::create_dir_all(dir).context("Failed to create temp directory")?;
        tracing::info!("Using temp directory for transcoding: {:?}", dir);
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
        let input_path = match &file.relative_path {
            Some(rel) => ctx.resolve_path(rel),
            None => PathBuf::from(&file.path),
        };

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

        diesel::update(files::table.find(file.id))
            .set(files::transcode_status.eq(TranscodeStatus::Transcoding))
            .execute(&mut conn)
            .await?;

        let result = match transcoder.transcode(&input_path, file.content_start_ms, file.content_end_ms, temp_dir.as_deref()) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Transcode failed: {}", e);
                transcoder.cleanup_failed(&Transcoder::transcoding_path(&input_path, temp_dir.as_deref())).ok();
                diesel::update(files::table.find(file.id))
                    .set(files::transcode_status.eq(TranscodeStatus::Failed))
                    .execute(&mut conn)
                    .await?;
                fail_count += 1;
                continue;
            }
        };

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

        if let Err(e) = transcoder.finalize(&result, &input_path) {
            tracing::error!("Finalize failed: {}", e);
            diesel::update(files::table.find(file.id))
                .set(files::transcode_status.eq(TranscodeStatus::Failed))
                .execute(&mut conn)
                .await?;
            fail_count += 1;
            continue;
        }

        // Update both absolute and relative paths to new file
        let new_relative = ctx.make_relative(&result.final_path).ok();

        diesel::update(files::table.find(file.id))
            .set((
                files::transcode_status.eq(TranscodeStatus::Completed),
                files::transcoded_path.eq(result.final_path.to_string_lossy().to_string()),
                files::transcoded_at.eq(Utc::now()),
                files::original_size_bytes.eq(result.original_size),
                files::path.eq(result.final_path.to_string_lossy().to_string()),
                files::relative_path.eq(new_relative),
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
