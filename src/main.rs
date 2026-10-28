use std::{fs, sync::{Arc, Mutex}, path::{Path, PathBuf}, collections::HashMap};
use of_client::{reqwest_cookie_store::{CookieStore, CookieStoreRwLock}, OFClient, RequestHeaders};
use reqwest::Url;
use cookie::Cookie;
use anyhow::{anyhow, Context};
use log::*;
use clap::{Parser, Subcommand};
use chrono::{DateTime, Utc};

mod tui;
mod downloader;
mod config;
mod db;
mod metadata;
mod backfill;
mod updater;

use tui::app::{log_message, ScrapeState, LikeState, log_like_message};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Scrape a user
    Scrape {
        /// Username or User ID
        ///
        /// Use ALL_PURCHASES as the user to download purchased content
        /// from every creator in your account.
        user: String,
        /// Type of content to scrape (posts, chats, stories, highlights, labels, purchases, all)
        #[arg(short, long, default_value = "all")]
        content_type: String,
        /// Re-download media even if the download database already has it
        /// (bypasses Download.SkipExistingMedia for this run)
        #[arg(long)]
        force: bool,
        /// Walk the API and fill the metadata database without downloading
        /// anything
        #[arg(long)]
        metadata_only: bool,
    },
    /// List subscriptions
    List,
    /// Like or unlike all of a creator's content
    Like {
        /// Username or User ID
        user: String,
        /// Type of content to affect (posts, chats, stories, all)
        #[arg(short, long, default_value = "posts")]
        content_type: String,
        /// Unlike instead of like
        #[arg(long)]
        unlike: bool,
    },
    /// Download a single post or message from a link, instead of
    /// scraping a whole creator. Supports two link shapes:
    /// post — https://onlyfans.com/{postId}/{username}
    /// message — https://onlyfans.com/my/chats/chat/{userId}/?firstId={messageId}
    Link {
        /// The post or message link to download
        url: String,
        /// Re-download even if the download database already has it
        #[arg(long)]
        force: bool,
        /// Record metadata in the user_data.db without downloading
        #[arg(long)]
        metadata_only: bool,
    },
    /// Download-database management (no OnlyFans auth needed)
    Db {
        #[command(subcommand)]
        action: DbCommand,
    },
    /// Check GitHub Releases for a newer version (prints a link; never
    /// downloads or replaces anything)
    CheckUpdate,
}

#[derive(Subcommand)]
enum DbCommand {
    /// Where the download database lives
    Path,
    /// Show download database statistics
    Stats {
        /// Restrict to one creator (username as stored in the db)
        #[arg(short, long)]
        user: Option<String>,
    },
    /// List recorded downloads, newest first
    List {
        /// Restrict to one creator
        #[arg(short, long)]
        user: Option<String>,
        /// How many entries to show
        #[arg(long, default_value_t = 20)]
        limit: u64,
        /// Skip the first N entries
        #[arg(long, default_value_t = 0)]
        offset: u64,
    },
    /// Delete recorded downloads (optionally just one creator's)
    Clear {
        /// Restrict to one creator; omit to wipe everything
        #[arg(short, long)]
        user: Option<String>,
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// Import already-downloaded files into the database
    ///
    /// Only useful when your filename formats contain {mediaId}: ids are
    /// extracted by matching the File.*FileNameFormat templates. Files saved
    /// under the default names can't be matched (ids aren't in their names)
    /// and are reported as such; they're still covered by the If-Modified-Since
    /// skip the downloader does regardless.
    Backfill {
        /// Restrict to one creator's directory under the download path
        #[arg(short, long)]
        user: Option<String>,
        /// Root of your content tree (default: DownloadPath from config.conf)
        #[arg(long)]
        download_path: Option<String>,
    },
}

pub struct AuthParams {
	pub cookie: CookieStore,
	pub user_id: String,
	pub x_bc: String,
	pub user_agent: String,
}

impl From<AuthParams> for RequestHeaders {
	fn from(value: AuthParams) -> Self {
		Self {
			cookie: Arc::new(CookieStoreRwLock::new(value.cookie)),
			user_id: value.user_id,
			user_agent: value.user_agent,
			x_bc: value.x_bc
		}
	}
}

pub struct CustomConfig {
    pub values: HashMap<String, String>,
}

impl CustomConfig {
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let mut values = HashMap::new();
        let content = fs::read_to_string(path)?;
        let mut current_block: Vec<String> = Vec::new();

        for line in content.lines() {
            let mut line = line.trim();
            if line.is_empty() {
                continue;
            }

            if let Some(pos) = line.find('#') {
                line = line[..pos].trim();
            }
            if let Some(pos) = line.find("//") {
                line = line[..pos].trim();
            }

            if line.is_empty() {
                continue;
            }

            if line == "}" {
                current_block.pop();
                continue;
            }

            if let Some(block_name) = line.strip_suffix('{') {
                current_block.push(block_name.trim().to_lowercase());
                continue;
            }

            if line == "{" {
                continue;
            }

            if let Some(pos) = line.find('=') {
                let key = line[..pos].trim().to_lowercase();
                let value = line[pos + 1..].trim()
                    .trim_matches(|c| c == '"' || c == '\'');

                let mut full_key = String::new();
                for block in &current_block {
                    full_key.push_str(block);
                    full_key.push('.');
                }
                full_key.push_str(&key);
                values.insert(full_key, value.to_string());
            }
        }

        Ok(Self { values })
    }

    pub fn get(&self, section: &str, key: &str) -> Option<&String> {
        let full_key = format!("{}.{}", section.to_lowercase(), key.to_lowercase());
        self.values.get(&full_key)
    }
}

fn init_file_logging() -> anyhow::Result<PathBuf> {
    let log_path = resolve_config_path("of-scraper-rs.log");
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::OpenOptions::new().create(true).append(true).open(&log_path)?;

    let level = config::config().logging_level;
    simplelog::WriteLogger::init(level, simplelog::Config::default(), file)
        .map_err(|e| anyhow!("Failed to initialize logger: {e}"))?;

    info!("=== of-scraper-rs v{} starting ===", env!("CARGO_PKG_VERSION"));
    Ok(log_path)
}

pub(crate) fn resolve_config_path(filename: &str) -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(filename);
        if candidate.exists() {
            return candidate;
        }
    }

    let cwd_candidate = PathBuf::from(filename);
    if cwd_candidate.exists() {
        return cwd_candidate;
    }

    if let Some(config_dir) = dirs::config_dir() {
        let candidate = config_dir.join("of-scraper-rs").join(filename);
        if candidate.exists() {
            return candidate;
        }
        return candidate; // doesn't exist yet, but this is where it should live
    }

    cwd_candidate
}

fn load_auth() -> anyhow::Result<AuthParams> {
    let auth_json = resolve_config_path("auth.json");
    let config_conf = resolve_config_path("config.conf");

    if auth_json.exists() {
        return load_auth_json(&auth_json);
    } else if config_conf.exists() {
        return load_config_conf(&config_conf);
    }
    Err(anyhow!(
        "Neither auth.json nor config.conf found. Checked next to the binary, the current \
         directory, and {}",
        dirs::config_dir().map(|d| d.join("of-scraper-rs").display().to_string()).unwrap_or_default()
    ))
}

fn load_auth_json(path: &Path) -> anyhow::Result<AuthParams> {
    #[derive(serde::Deserialize)]
    struct AuthFileInner {
        cookie: String,
        user_agent: String,
        x_bc: String,
    }
    #[derive(serde::Deserialize)]
    struct AuthFile { auth: AuthFileInner }

    let data = fs::read_to_string(path)?;
    let parsed: AuthFile = serde_json::from_str(&data)?;
    let inner = parsed.auth;

    let mut store = CookieStore::new(None);
    let url: Url = "https://onlyfans.com".parse().unwrap();
    for cookie_str in Cookie::split_parse(inner.cookie) {
        let cookie = cookie_str.map_err(|e| anyhow!("Cookie parse error: {}", e))?;
        store.insert_raw(&cookie, &url).map_err(|e| anyhow!("Cookie store error: {}", e))?;
    }

    let user_id = store.get_any(url.domain().unwrap(), url.path(), "auth_id")
        .map(|c| c.value().to_string())
        .ok_or_else(|| anyhow!("auth_id missing from cookies"))?;

    Ok(AuthParams {
        cookie: store,
        user_id,
        x_bc: inner.x_bc,
        user_agent: inner.user_agent,
    })
}

fn load_config_conf(path: &Path) -> anyhow::Result<AuthParams> {
    let config = CustomConfig::load(path).map_err(|e| anyhow!("Config load error: {}", e))?;

    let sess = config.get("auth", "sess")
        .cloned()
        .ok_or_else(|| anyhow!("'sess' missing from [auth] in config.conf"))?;
    let auth_id = config.get("auth", "auth_id")
        .cloned()
        .ok_or_else(|| anyhow!("'auth_id' missing from [auth] in config.conf"))?;
    let user_agent = config.get("auth", "user_agent")
        .cloned()
        .ok_or_else(|| anyhow!("'user_agent' missing from [auth] in config.conf"))?;
    let x_bc = config.get("auth", "x_bc")
        .cloned()
        .ok_or_else(|| anyhow!("'x_bc' missing from [auth] in config.conf"))?;

    let mut store = CookieStore::new(None);
    let url: Url = "https://onlyfans.com".parse().unwrap();

    let sess_cookie = Cookie::build(("sess", sess)).domain("onlyfans.com").build();
    let auth_id_cookie = Cookie::build(("auth_id", &auth_id)).domain("onlyfans.com").build();

    store.insert_raw(&sess_cookie, &url).map_err(|e| anyhow!("Cookie store error (sess): {}", e))?;
    store.insert_raw(&auth_id_cookie, &url).map_err(|e| anyhow!("Cookie store error (auth_id): {}", e))?;

    Ok(AuthParams {
        cookie: store,
        user_id: auth_id,
        x_bc,
        user_agent,
    })
}

fn load_cdm() -> Option<of_client::widevine::Cdm> {
    let wvd_path = resolve_config_path("device.wvd");
    if wvd_path.exists() {
        match fs::File::open(&wvd_path) {
            Ok(file) => {
                match of_client::widevine::Device::read_wvd(file) {
                    Ok(device) => {
                        info!("Successfully loaded Widevine device");
                        Some(of_client::widevine::Cdm::new(device))
                    }
                    Err(e) => {
                        error!("Failed to parse device.wvd: {}", e);
                        None
                    }
                }
            }
            Err(e) => {
                error!("Failed to open device.wvd: {}", e);
                None
            }
        }
    } else {
        debug!("device.wvd not found, DRM decryption will not be available");
        None
    }
}

fn get_download_path() -> PathBuf {
    match &config::config().download.download_path {
        Some(p) if !p.as_os_str().is_empty() => p.clone(),
        _ => PathBuf::from("data"),
    }
}

async fn queue_downloads<T>(
    downloader: &Arc<downloader::Downloader>,
    semaphore: &Arc<tokio::sync::Semaphore>,
    state_opt: &Option<Arc<Mutex<ScrapeState>>>,
    content: &T,
    username: &str,
    download_path: &Path,
    subfolder: Option<&str>,
) -> Vec<tokio::task::JoinHandle<()>> where
    T: of_client::content::Content + of_client::content::HasMedia<Media = of_client::media::Feed> + Send + Sync + 'static,
{
    use of_client::media::Media;
    use of_client::media::MediaType;

    let content_type_str = T::content_type().to_string();
    let content_id = content.id();
    let content_is_paid = content.is_paid();
    let content_text = content.text().to_string();
    let content_bucket = content.filename_format_bucket();

    let mut content_path = download_path.join(username).join(&content_type_str);
    if let Some(sub) = subfolder {
        // Label/highlight titles can contain characters that are awkward
        // in paths (slashes, etc); keep it simple and just strip those.
        let sanitized: String = sub.chars().filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')).collect();
        content_path = content_path.join(sanitized);
    }

    // Folder.FolderPer*Post/Message — one subfolder per content item.
    if config::folder_per_content(content_bucket, content_is_paid) {
        let ts = content.timestamp();
        content_path = content_path.join(format!("{}_{}", ts.format("%Y-%m-%d"), content_id));
    }

    let mut handles = Vec::new();

    // Metadata capture for the content row itself (text/price/date/author)
    // happens before any media-level filtering, so a metadata-only run
    // records text-only posts too.
    if metadata::capture_enabled() {
        let (capture_user_id, capture_username) = content
            .metadata_user()
            .map(|(id, name)| (Some(id), name))
            .unwrap_or((None, ""));
        metadata::record_content(
            download_path,
            username,
            metadata::ContentMeta {
                content_id,
                text: &content_text,
                price: content.price(),
                paid: content_is_paid,
                archived: false,
                created_at: content.timestamp(),
                api_type: &content_type_str,
                user_id: capture_user_id,
                username: capture_username,
            },
        );
    }

    for media in content.media().iter().cloned() {
        // Aborted run: stop before queueing anything new.
        if let Some(state) = &state_opt
            && state.lock().unwrap().should_quit
        {
            break;
        }
        let downloader = downloader.clone();
        let semaphore = semaphore.clone();
        let state_opt = state_opt.clone();
        let media_type = *media.media_type();

        if !config::media_type_enabled(media_type) {
            let msg = format!(
                "Skipping media {} ({}): disabled by config.conf Download.Media toggles",
                media.id, config::media_type_name(media_type)
            );
            warn!("{}", msg);
            log_message(&state_opt, &msg);
            continue;
        }

        let is_drm = media.drm().is_some();
        let media_id = media.id;

        if !is_drm && media.source().is_none() {
            let msg = format!(
                "Skipping media {} from content {} ({}): no source URL and not DRM — likely still locked despite appearing in this listing",
                media_id, content_id, content_type_str
            );
            warn!("{}", msg);
            log_message(&state_opt, &msg);
            continue;
        }

        if config::should_skip_duplicates() {
            let mut seen = config::seen_media().lock().unwrap();
            if !seen.insert(media_id) {
                drop(seen);
                let msg = format!(
                    "Skipping media {}: already downloaded earlier this run (DownloadDuplicatedMedia = false)",
                    media_id
                );
                debug!("{}", msg);
                log_message(&state_opt, &msg);
                continue;
            }
        }

        let media_dir = content_path.join(match media_type {
            MediaType::Photo => "Images",
            MediaType::Audio => "Audios",
            MediaType::Video | MediaType::Gif => "Videos",
        });

        let original_filename = if let Some(url_str) = media.source() {
            if let Ok(url) = Url::parse(url_str) {
                url.path_segments()
                    .and_then(|mut s| s.next_back())
                    .unwrap_or("file")
                    .to_string()
            } else {
                "file".to_string()
            }
        } else if is_drm {
            format!("{}.mp4", media_id)
        } else {
            format!("{}", media_id)
        };

        let final_filename = config::format_download_filename(
            content_bucket,
            content_is_paid,
            content_id,
            content.timestamp(),
            &content_text,
            username,
            &media,
            &original_filename,
        )
        .unwrap_or_else(|| original_filename.clone());

        // Metadata row for this media — placed after the filename is known
        // but before the download skips, so even not-downloaded media is
        // recorded where the file would live.
        if metadata::capture_enabled() {
            let media_record = metadata::MediaMeta {
                media_id,
                post_id: content_id,
                link: media.source(),
                directory: metadata::relative_download_directory(download_path, &media_dir),
                filename: final_filename.clone(),
                size: fs::metadata(media_dir.join(&final_filename)).ok().map(|meta| meta.len() as i64),
                api_type: &content_type_str,
                media_type: metadata::media_type_str(media_type),
                preview: !media.can_view(),
                downloaded: false,
                created_at: content.timestamp(),
            };
            metadata::record_media(download_path, username, media_record);
        }

        // --metadata-only: every byte of metadata above is captured, but no
        // download task is spawned — the API walk stops at the database.
        if config::metadata_only() {
            let msg = format!("Captured metadata for media {} (metadata-only, not downloading)", media_id);
            debug!("{}", msg);
            log_message(&state_opt, &msg);
            continue;
        }

        // Cross-run skip: the download database knows this id from a previous
        // run (or it was backfilled). `--force` bypasses it. Checked after the
        // metadata capture so metadata-only runs still see everything.
        if config::skip_existing_media() {
            match db::has_media(media_id) {
                Ok(true) => {
                    let msg = format!(
                        "Skipping media {}: already recorded in the download database ({})",
                        media_id,
                        db::path().display()
                    );
                    debug!("{}", msg);
                    log_message(&state_opt, &msg);
                    continue;
                }
                Ok(false) => {}
                Err(e) => warn!("Download db unavailable, not skipping media {}: {}", media_id, e),
            }
        }


        // still sits at the default name and hasn't been re-saved under the
        // custom format yet, move it over. DRM files can't be matched this
        // way (their old name only shows up in the MPD), so skip those.
        if !is_drm && config::config().file.rename_existing_files_when_custom_format_is_selected {
            let old_path = media_dir.join(&original_filename);
            let new_path = media_dir.join(&final_filename);
            if old_path != new_path && old_path.exists() && !new_path.exists() {
                match fs::rename(&old_path, &new_path) {
                    Ok(_) => log_message(
                        &state_opt,
                        &format!(
                            "Renamed {} → {} (custom filename format)",
                            old_path.file_name().unwrap_or_default().to_string_lossy(),
                            new_path.file_name().unwrap_or_default().to_string_lossy()
                        ),
                    ),
                    Err(e) => warn!(
                        "Could not rename {} to {}: {}",
                        old_path.display(),
                        new_path.display(),
                        e
                    ),
                }
            }
        }

        if let Some(state) = &state_opt {
            state.lock().unwrap().pending_downloads += 1;
        }

        let creator_name = username.to_string();
        let creator_for_db = creator_name.clone();
        let content_type_for_db = content_type_str.clone();
        let download_path_for_db = download_path.to_path_buf();
        let drm_type_str = content.drm_type_str();
        let path = media_dir.clone();

        let handle = tokio::spawn(async move {
            // Aborted before this task even started: drop out without
            // touching the queue. unstarted waiters drain via this check.
            if let Some(state) = &state_opt
                && state.lock().unwrap().should_quit
            {
                state.lock().unwrap().pending_downloads -= 1;
                log_message(&state_opt, &format!("Skipped download of {} (scrape aborted)", final_filename));
                return;
            }

            let _permit = match semaphore.acquire().await {
                Ok(p) => p,
                Err(_) => {
                    if let Some(state) = &state_opt {
                        state.lock().unwrap().pending_downloads -= 1;
                    }
                    return;
                }
            };

            // It may also have been queued long before the abort and is
            // only getting its per-slot now: still skip.
            if let Some(state) = &state_opt
                && state.lock().unwrap().should_quit
            {
                state.lock().unwrap().pending_downloads -= 1;
                log_message(&state_opt, &format!("Skipped download of {} (scrape aborted)", final_filename));
                return;
            }

            log_message(&state_opt, &format!("Starting download of {}", final_filename));

            let res = if is_drm {
                let license_url = format!(
                    "https://onlyfans.com/api2/v2/users/media/{}/drm/{}/{}?type=widevine",
                    media_id, drm_type_str, content_id
                );
                if let Some(drm) = media.drm() {
                    downloader.download_media_drm_tracked(drm, &license_url, &path, media_id, &final_filename, state_opt.clone(), creator_name).await
                } else {
                    Err(anyhow!("DRM manifest missing"))
                }
            } else {
                downloader.download_media_tracked(&media, &path, &final_filename, state_opt.clone(), creator_name).await
            };

            match res {
                Ok(_) => {
                    log_message(&state_opt, &format!("Successfully downloaded {}", final_filename));
                    // The metadata db learns the file really exists now
                    // (size included); it doesn't belong to the download
                    // task's failure domain, so its error is warning-only.
                    let downloaded_size = fs::metadata(path.join(&final_filename)).ok().map(|meta| meta.len() as i64);
                    metadata::mark_downloaded(&download_path_for_db, &creator_for_db, media_id, downloaded_size);
                    // The db is the cross-run memory for SkipExistingMedia and
                    // `db stats`, so record every success regardless of the
                    // skip setting.
                    let record = db::MediaRecord {
                        media_id,
                        creator: creator_for_db,
                        media_type: config::media_type_name(media_type).to_string(),
                        content_type: content_type_for_db,
                        downloaded_at: chrono::Utc::now(),
                    };
                    if let Err(e) = db::record(&record) {
                        warn!("Could not record media {} in download db: {}", media_id, e);
                    }
                }
                Err(e) => log_message(&state_opt, &format!("Failed to download {}: {}", final_filename, e)),
            }

            if let Some(state) = &state_opt {
                state.lock().unwrap().pending_downloads -= 1;
            }
        });

        handles.push(handle);
    }
    handles
}

pub async fn run_scrape_engine_tui(
    client: OFClient,
    downloader: Arc<downloader::Downloader>,
    username: String,
    content_types: Vec<String>,
    state: Arc<Mutex<ScrapeState>>,
) -> anyhow::Result<()> {
    {
        let mut s = state.lock().unwrap();
        s.username = username.clone();
    }
    log_message(&Some(state.clone()), &format!("Fetching user @{}...", username));
    let user: Option<of_client::user::User> = match client.get_user(username.as_str()).await {
        Ok(u) => {
            log_message(&Some(state.clone()), &format!("User found: {} (ID: {})", u.name, u.id));
            Some(u)
        }
        Err(e) => {
            log_message(&Some(state.clone()), &format!(
                "Could not resolve user @{}: {} (account may be deleted/banned) — skipping id-based content types, will still try purchases if selected",
                username, e
            ));
            None
        }
    };

    let download_path = get_download_path();
    let semaphore = Arc::new(tokio::sync::Semaphore::new(4));

    for t in &content_types {
        if state.lock().unwrap().should_quit { break; }

        if !config::content_type_enabled(t) {
            log_message(&Some(state.clone()), &format!(
                "Skipping {}: disabled by config.conf Download.Media toggles",
                t
            ));
            continue;
        }

        match t.as_str() {
            "posts" => {
                let Some(user) = user.as_ref() else {
                    log_message(&Some(state.clone()), "Skipping posts: user could not be resolved");
                    continue;
                };
                let mut before: Option<DateTime<Utc>> = None;
                loop {
                    if state.lock().unwrap().should_quit { break; }

                    state.lock().unwrap().status = format!("Scraping posts (before: {:?})...", before);
                    let posts = match client.get_posts(user.id, before).await {
                        Ok(p) => p,
                        Err(e) => {
                            log_message(&Some(state.clone()), &format!("Failed to get posts: {}", e));
                            break;
                        }
                    };
                    if posts.is_empty() { break; }

                    let mut count = 0;
                    for post in &posts {
                        if state.lock().unwrap().should_quit { break; }
                        before = Some(post.posted_at);
                        count += 1;

                        if config::should_skip_post(post) {
                            log_message(&Some(state.clone()), &format!("Skipping post {} (filtered by config)", post.id));
                            continue;
                        }
                        match config::post_date_filter(post.posted_at) {
                            config::DateFilter::Keep => {}
                            config::DateFilter::Skip => continue,
                            config::DateFilter::Break => break,
                        }

                        state.lock().unwrap().posts_scraped += 1;

                        queue_downloads(&downloader, &semaphore, &Some(state.clone()), post, &user.username, &download_path, None).await;
                    }

                    if count < 10 { break; }
                }
            }
            "chats" => {
                let Some(user) = user.as_ref() else {
                    log_message(&Some(state.clone()), "Skipping chats: user could not be resolved");
                    continue;
                };
                let mut before_id: Option<u64> = None;
                loop {
                    if state.lock().unwrap().should_quit { break; }

                    state.lock().unwrap().status = format!("Scraping chats (before_id: {:?})...", before_id);
                    let chats = match client.get_chats(user.id, before_id).await {
                        Ok(c) => c,
                        Err(e) => {
                            log_message(&Some(state.clone()), &format!("Failed to get chats: {}", e));
                            break;
                        }
                    };
                    if chats.is_empty() { break; }

                    let mut count = 0;
                    for chat in &chats {
                        if state.lock().unwrap().should_quit { break; }
                        before_id = Some(chat.id);
                        count += 1;

                        if config::should_skip_chat(chat) {
                            log_message(&Some(state.clone()), &format!("Skipping message {} (filtered by config)", chat.id));
                            continue;
                        }

                        state.lock().unwrap().chats_scraped += 1;

                        queue_downloads(&downloader, &semaphore, &Some(state.clone()), chat, &user.username, &download_path, None).await;
                    }

                    if count < 10 { break; }
                }
            }
            "stories" => {
                let Some(user) = user.as_ref() else {
                    log_message(&Some(state.clone()), "Skipping stories: user could not be resolved");
                    continue;
                };
                if state.lock().unwrap().should_quit { break; }

                state.lock().unwrap().status = "Scraping stories...".to_string();
                match client.get_stories(user.id).await {
                    Ok(stories) => {
                        for story in &stories {
                            if state.lock().unwrap().should_quit { break; }

                            state.lock().unwrap().stories_scraped += 1;

                            queue_downloads(&downloader, &semaphore, &Some(state.clone()), story, &user.username, &download_path, None).await;
                        }
                    }
                    Err(e) => {
                        log_message(&Some(state.clone()), &format!("Failed to get stories: {}", e));
                    }
                }
            }
            "highlights" => {
                let Some(user) = user.as_ref() else {
                    log_message(&Some(state.clone()), "Skipping highlights: user could not be resolved");
                    continue;
                };
                let mut offset: u64 = 0;
                loop {
                    if state.lock().unwrap().should_quit { break; }

                    state.lock().unwrap().status = format!("Scraping highlights (offset: {})...", offset);
                    let (summaries, has_more) = match client.get_highlights(user.id, offset).await {
                        Ok(v) => v,
                        Err(e) => {
                            log_message(&Some(state.clone()), &format!("Failed to get highlights: {}", e));
                            break;
                        }
                    };
                    if summaries.is_empty() { break; }

                    for summary in &summaries {
                        if state.lock().unwrap().should_quit { break; }

                        match client.get_highlight(summary.id).await {
                            Ok(highlight) => {
                                state.lock().unwrap().highlights_scraped += 1;
                                queue_downloads(&downloader, &semaphore, &Some(state.clone()), &highlight, &user.username, &download_path, None).await;
                            }
                            Err(e) => log_message(&Some(state.clone()), &format!("Failed to get highlight '{}': {}", summary.title, e)),
                        }
                    }

                    offset += 5;
                    if !has_more { break; }
                }
            }
            "labels" => {
                let Some(user) = user.as_ref() else {
                    log_message(&Some(state.clone()), "Skipping labels: user could not be resolved");
                    continue;
                };
                let mut label_offset: u64 = 0;
                loop {
                    if state.lock().unwrap().should_quit { break; }

                    let (labels, labels_has_more) = match client.get_labels(user.id, label_offset).await {
                        Ok(v) => v,
                        Err(e) => {
                            log_message(&Some(state.clone()), &format!("Failed to get labels: {}", e));
                            break;
                        }
                    };
                    if labels.is_empty() { break; }

                    for label in &labels {
                        if config::is_archive_label(&label.name) && !config::archive_label_enabled() {
                            log_message(&Some(state.clone()), &format!(
                                "Skipping label '{}': Archive downloads disabled by config.conf",
                                label.name
                            ));
                            continue;
                        }
                        let mut before: Option<DateTime<Utc>> = None;
                        loop {
                            if state.lock().unwrap().should_quit { break; }

                            state.lock().unwrap().status = format!("Scraping label '{}' (before: {:?})...", label.name, before);
                            let posts = match client.get_posts_by_label(user.id, &label.id, before).await {
                                Ok(p) => p,
                                Err(e) => {
                                    log_message(&Some(state.clone()), &format!("Failed to get posts for label '{}': {}", label.name, e));
                                    break;
                                }
                            };
                            if posts.is_empty() { break; }

                            let mut count = 0;
                            for post in &posts {
                                if state.lock().unwrap().should_quit { break; }
                                before = Some(post.posted_at);
                                count += 1;

                                if config::should_skip_post(post) {
                                    log_message(&Some(state.clone()), &format!("Skipping post {} (filtered by config)", post.id));
                                    continue;
                                }
                                match config::post_date_filter(post.posted_at) {
                                    config::DateFilter::Keep => {}
                                    config::DateFilter::Skip => continue,
                                    config::DateFilter::Break => break,
                                }

                                state.lock().unwrap().posts_scraped += 1;

                                queue_downloads(&downloader, &semaphore, &Some(state.clone()), post, &user.username, &download_path, Some(&label.name)).await;
                            }

                            if count < 10 { break; }
                        }
                    }

                    label_offset += 10;
                    if !labels_has_more { break; }
                }
            }
            "purchases" => {
                let mut offset: u64 = 0;
                loop {
                    if state.lock().unwrap().should_quit { break; }

                    state.lock().unwrap().status = format!("Scraping purchased content (offset: {})...", offset);
                    let (purchases, has_more) = match client.get_purchased_content(Some(&username), offset).await {
                        Ok(p) => p,
                        Err(e) => {
                            log_message(&Some(state.clone()), &format!("Failed to get purchased content: {}", e));
                            break;
                        }
                    };
                    if purchases.is_empty() { break; }

                    for purchase in &purchases {
                        if state.lock().unwrap().should_quit { break; }

                        state.lock().unwrap().purchases_scraped += 1;

                        queue_downloads(&downloader, &semaphore, &Some(state.clone()), purchase, &username, &download_path, None).await;
                    }

                    offset += 10;
                    if !has_more { break; }
                }
            }
            _ => log_message(&Some(state.clone()), &format!("Content type '{}' not recognized", t)),
        }
    }

    state.lock().unwrap().status = "Finishing downloads...".to_string();
    log_message(&Some(state.clone()), "Scraping loop finished. Waiting for active downloads to complete...");

    // Drain before declaring finished — including after a TUI abort, where
    // unstarted queued downloads exit via their should_quit checks and the
    // handful of in-flight ones are allowed to finish.
    while state.lock().unwrap().pending_downloads != 0 {
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    state.lock().unwrap().is_finished = true;
    log_message(&Some(state.clone()), "All operations finished.");
    Ok(())
}

pub async fn run_scrape_all_purchases_tui(
    client: OFClient,
    downloader: Arc<downloader::Downloader>,
    state: Arc<Mutex<ScrapeState>>,
) -> anyhow::Result<()> {
    let download_path = get_download_path();
    let semaphore = Arc::new(tokio::sync::Semaphore::new(4));
    let mut offset: u64 = 0;

    loop {
        if state.lock().unwrap().should_quit { break; }

        state.lock().unwrap().status = format!("Scraping all purchased content (offset: {})...", offset);
        let (purchases, has_more) = match client.get_purchased_content::<&str>(None, offset).await {
            Ok(p) => p,
            Err(e) => {
                log_message(&Some(state.clone()), &format!("Failed to get purchased content: {}", e));
                break;
            }
        };
        if purchases.is_empty() { break; }

        for purchase in &purchases {
            if state.lock().unwrap().should_quit { break; }
            state.lock().unwrap().purchases_scraped += 1;
            let author_username = purchase.author_username();
            queue_downloads(&downloader, &semaphore, &Some(state.clone()), purchase, &author_username, &download_path, None).await;
        }
        offset += 10;
        if !has_more { break; }
    }

    state.lock().unwrap().status = "Finishing downloads...".to_string();
    log_message(&Some(state.clone()), "Scraping loop finished. Waiting for active downloads to complete...");
    // Same drain-before-finish as the creator scrape (abort-safe).
    while state.lock().unwrap().pending_downloads != 0 {
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
    state.lock().unwrap().is_finished = true;
    log_message(&Some(state.clone()), "All operations finished.");
    Ok(())
}

const LIKE_REQUEST_DELAY: std::time::Duration = std::time::Duration::from_millis(800);

async fn apply_like<T: of_client::content::CanLike>(client: &OFClient, content: &T, liking: bool) -> of_client::reqwest_middleware::Result<bool> {
    let result = client.set_liked(content, liking).await;
    if !matches!(result, Ok(false)) {
        tokio::time::sleep(LIKE_REQUEST_DELAY).await;
    }
    result
}

pub async fn run_like_engine_tui(
    client: OFClient,
    username: String,
    content_types: Vec<String>,
    liking: bool,
    state: Arc<Mutex<LikeState>>,
) -> anyhow::Result<()> {
    let action = if liking { "Liking" } else { "Unliking" };
    log_like_message(&state, &format!("Fetching user @{}...", username));
    let user = match client.get_user(username.as_str()).await {
        Ok(u) => u,
        Err(e) => {
            let msg = format!("Failed to resolve user @{}: {} — cannot like/unlike without a numeric user id", username, e);
            log_like_message(&state, &msg);
            state.lock().unwrap().status = "Failed".to_string();
            state.lock().unwrap().is_finished = true;
            return Err(anyhow!(msg));
        }
    };
    log_like_message(&state, &format!("User found: {} (ID: {})", user.name, user.id));

    for t in &content_types {
        if state.lock().unwrap().should_quit { break; }

        match t.as_str() {
            "posts" => {
                let mut before: Option<DateTime<Utc>> = None;
                loop {
                    if state.lock().unwrap().should_quit { break; }

                    state.lock().unwrap().status = format!("{} posts (before: {:?})...", action, before);
                    let posts = match client.get_posts(user.id, before).await {
                        Ok(p) => p,
                        Err(e) => {
                            log_like_message(&state, &format!("Failed to get posts: {}", e));
                            break;
                        }
                    };
                    if posts.is_empty() { break; }

                    let mut count = 0;
                    for post in &posts {
                        if state.lock().unwrap().should_quit { break; }
                        state.lock().unwrap().posts_processed += 1;

                        match apply_like(&client, post, liking).await {
                            Ok(true) => {
                                state.lock().unwrap().changed += 1;
                                log_like_message(&state, &format!("{} post {}", if liking { "Liked" } else { "Unliked" }, post.id));
                            }
                            Ok(false) => { state.lock().unwrap().skipped += 1; }
                            Err(e) => {
                                state.lock().unwrap().failed += 1;
                                log_like_message(&state, &format!("Failed to update post {}: {}", post.id, e));
                            }
                        }

                        before = Some(post.posted_at);
                        count += 1;
                    }

                    if count < 10 { break; }
                }
            }
            "chats" => {
                let mut before_id: Option<u64> = None;
                loop {
                    if state.lock().unwrap().should_quit { break; }

                    state.lock().unwrap().status = format!("{} chats (before_id: {:?})...", action, before_id);
                    let chats = match client.get_chats(user.id, before_id).await {
                        Ok(c) => c,
                        Err(e) => {
                            log_like_message(&state, &format!("Failed to get chats: {}", e));
                            break;
                        }
                    };
                    if chats.is_empty() { break; }

                    let mut count = 0;
                    for chat in &chats {
                        if state.lock().unwrap().should_quit { break; }
                        state.lock().unwrap().chats_processed += 1;

                        match apply_like(&client, chat, liking).await {
                            Ok(true) => {
                                state.lock().unwrap().changed += 1;
                                log_like_message(&state, &format!("{} message {}", if liking { "Liked" } else { "Unliked" }, chat.id));
                            }
                            Ok(false) => { state.lock().unwrap().skipped += 1; }
                            Err(e) => {
                                state.lock().unwrap().failed += 1;
                                log_like_message(&state, &format!("Failed to update message {}: {}", chat.id, e));
                            }
                        }

                        before_id = Some(chat.id);
                        count += 1;
                    }

                    if count < 10 { break; }
                }
            }
            "stories" => {
                state.lock().unwrap().status = format!("{} stories...", action);
                match client.get_stories(user.id).await {
                    Ok(stories) => {
                        for story in &stories {
                            if state.lock().unwrap().should_quit { break; }
                            state.lock().unwrap().stories_processed += 1;

                            match apply_like(&client, story, liking).await {
                                Ok(true) => {
                                    state.lock().unwrap().changed += 1;
                                    log_like_message(&state, &format!("{} story {}", if liking { "Liked" } else { "Unliked" }, story.id));
                                }
                                Ok(false) => { state.lock().unwrap().skipped += 1; }
                                Err(e) => {
                                    state.lock().unwrap().failed += 1;
                                    log_like_message(&state, &format!("Failed to update story {}: {}", story.id, e));
                                }
                            }
                        }
                    }
                    Err(e) => log_like_message(&state, &format!("Failed to get stories: {}", e)),
                }
            }
            _ => log_like_message(&state, &format!("Content type '{}' not recognized", t)),
        }
    }

    state.lock().unwrap().is_finished = true;
    log_like_message(&state, "All operations finished.");
    Ok(())
}

/// What a copied OnlyFans link points at, after parsing.
enum LinkTarget {
    /// `https://onlyfans.com/{postId}/{username}`
    Post { post_id: u64 },
    /// `https://onlyfans.com/my/chats/chat/{userId}/?firstId={messageId}`
    Message { user_id: u64, message_id: u64 },
}

fn parse_content_link(input: &str) -> anyhow::Result<LinkTarget> {
    let url = Url::parse(input).map_err(|e| anyhow!("Not a valid URL: {e}"))?;
    let segments: Vec<&str> = url.path_segments().map(|s| s.collect()).unwrap_or_default();

    if let Some(pos) = segments.iter().position(|s| *s == "chat")
        && let Some(user_id_str) = segments.get(pos + 1)
        && let Ok(user_id) = user_id_str.parse::<u64>()
    {
        let message_id: u64 = url.query_pairs()
            .find(|(k, _)| k == "firstId")
            .map(|(_, v)| v.into_owned())
            .ok_or_else(|| anyhow!("Chat link is missing '?firstId=' — needed to identify the message"))?
            .parse()
            .map_err(|_| anyhow!("Couldn't parse the message id out of '?firstId='"))?;
        return Ok(LinkTarget::Message { user_id, message_id });
    }

    // Post link: onlyfans.com/{postId}/{username}
    if let Some(first) = segments.first()
        && let Ok(post_id) = first.parse::<u64>()
    {
        return Ok(LinkTarget::Post { post_id });
    }

    Err(anyhow!(
        "Couldn't recognize '{}' as a post link (onlyfans.com/{{postId}}/{{username}}) \
         or a message link (onlyfans.com/my/chats/chat/{{userId}}/?firstId={{messageId}})",
        input
    ))
}

pub async fn run_link_download_tui(
    client: OFClient,
    downloader: Arc<downloader::Downloader>,
    urls: Vec<String>,
    state: Arc<Mutex<ScrapeState>>,
) -> anyhow::Result<()> {
    let download_path = get_download_path();
    let semaphore = Arc::new(tokio::sync::Semaphore::new(4));
    let mut tasks = Vec::new();

    for url_str in urls {
        if state.lock().unwrap().should_quit { break; }
        let url_str = url_str.trim();
        if url_str.is_empty() { continue; }

        log_message(&Some(state.clone()), &format!("Parsing link: {}", url_str));
        let target = match parse_content_link(url_str) {
            Ok(t) => t,
            Err(e) => {
                log_message(&Some(state.clone()), &format!("Failed to parse link: {}", e));
                continue;
            }
        };

        match target {
            LinkTarget::Post { post_id } => {
                log_message(&Some(state.clone()), &format!("Fetching post {}...", post_id));
                match client.get_post(post_id).await {
                    Ok(post) => {
                        let username = post.author.username.clone();
                        state.lock().unwrap().posts_scraped += 1;
                        tasks.extend(queue_downloads(&downloader, &semaphore, &Some(state.clone()), &post, &username, &download_path, None).await);
                    }
                    Err(e) => log_message(&Some(state.clone()), &format!("Failed to fetch post: {}", e)),
                }
            }
            LinkTarget::Message { user_id, message_id } => {
                log_message(&Some(state.clone()), &format!("Looking for message {}...", message_id));
                let username = client.get_user(user_id).await.map(|u| u.username).unwrap_or_else(|_| user_id.to_string());
                
                let mut before_id: Option<u64> = None;
                let mut found = false;
                for page in 0..200 {
                    if state.lock().unwrap().should_quit { break; }
                    let chats = match client.get_chats(user_id, before_id).await {
                        Ok(c) => c,
                        Err(e) => {
                            log_message(&Some(state.clone()), &format!("Failed to get chats: {}", e));
                            break;
                        }
                    };
                    if chats.is_empty() { break; }

                    if let Some(chat) = chats.iter().find(|c| c.id == message_id) {
                        state.lock().unwrap().chats_scraped += 1;
                        tasks.extend(queue_downloads(&downloader, &semaphore, &Some(state.clone()), chat, &username, &download_path, None).await);
                        found = true;
                        break;
                    }
                    before_id = chats.last().map(|c| c.id);
                    if page % 10 == 9 {
                        log_message(&Some(state.clone()), &format!("...still looking, scanned {} pages so far", page + 1));
                    }
                }
                if !found {
                    log_message(&Some(state.clone()), &format!("Couldn't find message {} in recent history", message_id));
                }
            }
        }
    }

    state.lock().unwrap().status = "Finishing downloads...".to_string();
    log_message(&Some(state.clone()), "Waiting for active downloads to complete...");
    
    for task in tasks {
        let _ = task.await;
    }

    state.lock().unwrap().is_finished = true;
    log_message(&Some(state.clone()), "All operations finished.");
    Ok(())
}

async fn download_link_cli(
    client: &OFClient,
    downloader: Arc<downloader::Downloader>,
    url: &str,
) -> anyhow::Result<()> {
    let target = parse_content_link(url)?;
    let download_path = get_download_path();
    let semaphore = Arc::new(tokio::sync::Semaphore::new(4));
    let mut tasks = Vec::new();

    match target {
        LinkTarget::Post { post_id } => {
            println!("Fetching post {post_id}...");
            let post = client.get_post(post_id).await.map_err(|e| anyhow!("Failed to fetch post: {e}"))?;
            println!("Post found, author @{} — queueing media", post.author.username);
            let username = post.author.username.clone();
            tasks.extend(queue_downloads(&downloader, &semaphore, &None, &post, &username, &download_path, None).await);
        }
        LinkTarget::Message { user_id, message_id } => {
            println!("Looking for message {message_id} in chat with user {user_id}...");

            let username = client.get_user(user_id).await
                .map(|u| u.username)
                .unwrap_or_else(|_| user_id.to_string());

            const MAX_PAGES: u32 = 200;
            let mut before_id: Option<u64> = None;
            let mut found = false;

            for page in 0..MAX_PAGES {
                let chats = client.get_chats(user_id, before_id).await.map_err(|e| anyhow!("Failed to get chats: {e}"))?;
                if chats.is_empty() { break; }

                if let Some(chat) = chats.iter().find(|c| c.id == message_id) {
                    println!("Found message {message_id} on page {} — queueing media", page + 1);
                    tasks.extend(queue_downloads(&downloader, &semaphore, &None, chat, &username, &download_path, None).await);
                    found = true;
                    break;
                }

                before_id = chats.last().map(|c| c.id);
                if page % 10 == 9 {
                    println!("...still looking, scanned {} pages so far", page + 1);
                }
            }

            if !found {
                return Err(anyhow!(
                    "Couldn't find message {message_id} after scanning {MAX_PAGES} pages of chat history"
                ));
            }
        }
    }

    println!("Waiting for active downloads to complete...");
    //let _permits = semaphore.acquire_many(4).await?;
    for task in tasks {
        let _ = task.await;
    }
    println!("Done.");
    Ok(())
}

async fn like_user_cli(
    client: &OFClient,
    username: &str,
    content_type: &str,
    liking: bool,
) -> anyhow::Result<()> {
    let action = if liking { "Liking" } else { "Unliking" };
    println!("{} @{}'s {}...", action, username, content_type);
    let user = client.get_user(username).await.map_err(|e| anyhow!("Failed to get user: {}", e))?;
    println!("User found: {} ({})", user.name, user.id);

    let types: Vec<&str> = match content_type {
        "all" => vec!["posts", "chats", "stories"],
        other => vec![other],
    };

    let (mut changed, mut skipped, mut failed) = (0u32, 0u32, 0u32);

    for t in types {
        match t {
            "posts" => {
                let mut before: Option<DateTime<Utc>> = None;
                loop {
                    let posts = client.get_posts(user.id, before).await.map_err(|e| anyhow!("Failed to get posts: {}", e))?;
                    if posts.is_empty() { break; }
                    let mut count = 0;
                    for post in &posts {
                        match apply_like(client, post, liking).await {
                            Ok(true) => { changed += 1; println!("{} post {}", if liking { "Liked" } else { "Unliked" }, post.id); }
                            Ok(false) => skipped += 1,
                            Err(e) => { failed += 1; println!("Failed to update post {}: {}", post.id, e); }
                        }
                        before = Some(post.posted_at);
                        count += 1;
                    }
                    if count < 10 { break; }
                }
            }
            "chats" => {
                let mut before_id: Option<u64> = None;
                loop {
                    let chats = client.get_chats(user.id, before_id).await.map_err(|e| anyhow!("Failed to get chats: {}", e))?;
                    if chats.is_empty() { break; }
                    let mut count = 0;
                    for chat in &chats {
                        match apply_like(client, chat, liking).await {
                            Ok(true) => { changed += 1; println!("{} message {}", if liking { "Liked" } else { "Unliked" }, chat.id); }
                            Ok(false) => skipped += 1,
                            Err(e) => { failed += 1; println!("Failed to update message {}: {}", chat.id, e); }
                        }
                        before_id = Some(chat.id);
                        count += 1;
                    }
                    if count < 10 { break; }
                }
            }
            "stories" => {
                match client.get_stories(user.id).await {
                    Ok(stories) => {
                        for story in &stories {
                            match apply_like(client, story, liking).await {
                                Ok(true) => { changed += 1; println!("{} story {}", if liking { "Liked" } else { "Unliked" }, story.id); }
                                Ok(false) => skipped += 1,
                                Err(e) => { failed += 1; println!("Failed to update story {}: {}", story.id, e); }
                            }
                        }
                    }
                    Err(e) => println!("Failed to get stories: {}", e),
                }
            }
            _ => println!("Content type '{}' not recognized", t),
        }
    }

    println!("Done. Changed: {} | Already correct: {} | Failed: {}", changed, skipped, failed);
    Ok(())
}

async fn scrape_user_cli(
    client: &OFClient,
    downloader: Arc<downloader::Downloader>,
    username: &str,
    content_type: &str,
) -> anyhow::Result<()> {
    use of_client::content::Content;

    if username == "ALL_PURCHASES" {
        println!("Scraping ALL purchased content...");
        let download_path = get_download_path();
        let semaphore = Arc::new(tokio::sync::Semaphore::new(4));
        let mut tasks = Vec::new();
        let mut offset: u64 = 0;
        loop {
            let (purchases, has_more) = client.get_purchased_content::<&str>(None, offset).await.map_err(|e| anyhow!("Failed to get purchased content: {}", e))?;
            if purchases.is_empty() { break; }
            for purchase in &purchases {
                println!("Queueing purchase {}", purchase.id());
                tasks.extend(queue_downloads(&downloader, &semaphore, &None, purchase, &purchase.author_username(), &download_path, None).await);
            }
            offset += 10;
            if !has_more { break; }
        }
        println!("Waiting for active downloads to complete...");
        //let _permits = semaphore.acquire_many(4).await?;
        for task in tasks {
            let _ = task.await;
        }
        println!("Scraping and downloads completed successfully.");
        return Ok(());
    }

    println!("Scraping @{} for {}...", username, content_type);
    let user = match client.get_user(username).await {
        Ok(u) => {
            println!("User found: {} ({})", u.name, u.id);
            Some(u)
        }
        Err(e) => {
            println!("Could not resolve user @{}: {} (account may be deleted/banned)", username, e);
            println!("Will still attempt purchases if that's what was requested.");
            None
        }
    };

    let download_path = get_download_path();
    let semaphore = Arc::new(tokio::sync::Semaphore::new(4));
    let mut tasks = Vec::new();

    let types = match content_type {
        "all" => vec!["posts", "chats", "stories"],
        "purchases" => vec!["purchases"],
        other => vec![other],
    };

    for t in types {
        if !config::content_type_enabled(t) {
            println!("Skipping {}: disabled by config.conf Download.Media toggles", t);
            continue;
        }
        match t {
            "posts" => {
                let Some(user) = user.as_ref() else { println!("Skipping posts: user could not be resolved"); continue };
                let mut before: Option<DateTime<Utc>> = None;
                loop {
                    let posts = client.get_posts(user.id, before).await.map_err(|e| anyhow!("Failed to get posts: {}", e))?;
                    if posts.is_empty() { break; }
                    let mut count = 0;
                    for post in &posts {
                        before = Some(post.posted_at);
                        count += 1;

                        if config::should_skip_post(post) {
                            println!("Skipping post {} (filtered by config)", post.id);
                            continue;
                        }
                        match config::post_date_filter(post.posted_at) {
                            config::DateFilter::Keep => {}
                            config::DateFilter::Skip => continue,
                            config::DateFilter::Break => break,
                        }

                        println!("Queueing post media from post {}", post.id);
                        tasks.extend(queue_downloads(&downloader, &semaphore, &None, post, &user.username, &download_path, None).await);
                    }
                    if count < 10 { break; }
                }
            }
            "chats" => {
                let Some(user) = user.as_ref() else { println!("Skipping chats: user could not be resolved"); continue };
                let mut before_id: Option<u64> = None;
                loop {
                    let chats = client.get_chats(user.id, before_id).await.map_err(|e| anyhow!("Failed to get chats: {}", e))?;
                    if chats.is_empty() { break; }
                    let mut count = 0;
                    for chat in &chats {
                        before_id = Some(chat.id);
                        count += 1;

                        if config::should_skip_chat(chat) {
                            println!("Skipping message {} (filtered by config)", chat.id);
                            continue;
                        }

                        println!("Queueing chat media from message {}", chat.id);
                        tasks.extend(queue_downloads(&downloader, &semaphore, &None, chat, &user.username, &download_path, None).await);
                    }
                    if count < 10 { break; }
                }
            }
            "stories" => {
                let Some(user) = user.as_ref() else { println!("Skipping stories: user could not be resolved"); continue };
                println!("Scraping active stories...");
                match client.get_stories(user.id).await {
                    Ok(stories) => {
                        for story in &stories {
                            tasks.extend(queue_downloads(&downloader, &semaphore, &None, story, &user.username, &download_path, None).await);
                        }
                    }
                    Err(e) => println!("Failed to get stories: {}", e),
                }
            }
            "highlights" => {
                let Some(user) = user.as_ref() else { println!("Skipping highlights: user could not be resolved"); continue };
                let mut offset: u64 = 0;
                loop {
                    let (summaries, has_more) = client.get_highlights(user.id, offset).await.map_err(|e| anyhow!("Failed to get highlights: {}", e))?;
                    if summaries.is_empty() { break; }
                    for summary in &summaries {
                        match client.get_highlight(summary.id).await {
                            Ok(highlight) => {
                                println!("Queueing highlight '{}'", highlight.title);
                                tasks.extend(queue_downloads(&downloader, &semaphore, &None, &highlight, &user.username, &download_path, None).await);
                            }
                            Err(e) => println!("Failed to get highlight '{}': {}", summary.title, e),
                        }
                    }
                    offset += 5;
                    if !has_more { break; }
                }
            }
            "labels" => {
                let Some(user) = user.as_ref() else { println!("Skipping labels: user could not be resolved"); continue };
                let mut label_offset: u64 = 0;
                loop {
                    let (labels, labels_has_more) = client.get_labels(user.id, label_offset).await.map_err(|e| anyhow!("Failed to get labels: {}", e))?;
                    if labels.is_empty() { break; }
                    for label in &labels {
                        if config::is_archive_label(&label.name) && !config::archive_label_enabled() {
                            println!("Skipping label '{}': Archive downloads disabled by config.conf", label.name);
                            continue;
                        }
                        let mut before: Option<DateTime<Utc>> = None;
                        loop {
                            let posts = client.get_posts_by_label(user.id, &label.id, before).await.map_err(|e| anyhow!("Failed to get posts for label '{}': {}", label.name, e))?;
                            if posts.is_empty() { break; }
                            let mut count = 0;
                            for post in &posts {
                                before = Some(post.posted_at);
                                count += 1;

                                if config::should_skip_post(post) {
                                    println!("Skipping post {} (filtered by config)", post.id);
                                    continue;
                                }
                                match config::post_date_filter(post.posted_at) {
                                    config::DateFilter::Keep => {}
                                    config::DateFilter::Skip => continue,
                                    config::DateFilter::Break => break,
                                }

                                println!("Queueing post {} from label '{}'", post.id, label.name);
                                tasks.extend(queue_downloads(&downloader, &semaphore, &None, post, &user.username, &download_path, Some(&label.name)).await);
                            }
                            if count < 10 { break; }
                        }
                    }
                    label_offset += 10;
                    if !labels_has_more { break; }
                }
            }
            "purchases" => {
                let mut offset: u64 = 0;
                loop {
                    let (purchases, has_more) = client.get_purchased_content(Some(username), offset).await.map_err(|e| anyhow!("Failed to get purchased content: {}", e))?;
                    if purchases.is_empty() { break; }
                    for purchase in &purchases {
                        println!("Queueing purchase {}", purchase.id());
                        tasks.extend(queue_downloads(&downloader, &semaphore, &None, purchase, username, &download_path, None).await);
                    }
                    offset += 10;
                    if !has_more { break; }
                }
            }
            _ => println!("Content type '{}' not recognized", t),
        }
    }

    println!("Waiting for active downloads to complete...");
    //let _permits = semaphore.acquire_many(4).await?;
    for task in tasks {
        let _ = task.await;
    }
    println!("Scraping and downloads completed successfully.");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load config.conf first so init_file_logging and everything else can
    // read its settings.
    let _cfg = config::config();

    let log_path = init_file_logging().context("Setting up logging")?;

    let cli = Cli::parse();

	// Update notice for one-shot commands (stderr, so stdout stays clean).
	// The TUI checks in the background and has its own notice; the explicit
	// check-update command doesn't need nudging.
	match &cli.command {
		Some(Commands::CheckUpdate) => {
			return run_check_update().await;
		}
		Some(_) => {
			if let Some(info) = updater::cached_notice() {
				eprintln!("{} — see {}", info.summary(), info.url);
			}
		}
		None => {}
	}

    // db management needs config.conf at most, never OnlyFans auth.
    if let Some(Commands::Db { action }) = cli.command {
        return run_db_command(action);
    }

    let auth = match load_auth().context("Loading authentication") {
        Ok(a) => a,
        Err(e) => {
            error!("Failed to load auth: {:#}", e);
            return Err(e);
        }
    };
    if let Ok(my_id) = auth.user_id.parse::<u64>() {
        config::set_my_user_id(my_id);
    }

    let timeout = config::config().performance.timeout;
    let client = OFClient::new_with_timeout(auth, timeout)?;

    let make_downloader = |client: OFClient| {
        let cdm = load_cdm();
        Arc::new(downloader::Downloader::new_with_ffmpeg(
            client,
            cdm,
            config::config().ffmpeg_path.clone(),
        ))
    };

    match cli.command {
        Some(Commands::Scrape { user, content_type, force, metadata_only }) => {
            config::set_force_downloads(force);
            config::set_metadata_only(metadata_only);
            let downloader = make_downloader(client.clone());
            scrape_user_cli(&client, downloader, &user, &content_type).await?;
        },
        Some(Commands::List) => {
            let cfg = config::config();
            let subs = client
                .get_subscriptions_filtered(
                    cfg.subscriptions.include_expired_subscriptions,
                    cfg.subscriptions.include_restricted_subscriptions,
                )
                .await
                .map_err(|e| anyhow!("Failed to get subscriptions: {}", e))?;
            for sub in subs {
                println!("- {} (@{})", sub.name, sub.username);
            }
        },
        Some(Commands::Like { user, content_type, unlike }) => {
            like_user_cli(&client, &user, &content_type, !unlike).await?;
        },
        Some(Commands::Link { url, force, metadata_only }) => {
            config::set_force_downloads(force);
            config::set_metadata_only(metadata_only);
            let downloader = make_downloader(client.clone());
            download_link_cli(&client, downloader, &url).await?;
        },
        // Db never reaches here — handled before auth setup.
        Some(Commands::Db { .. }) => unreachable!("db commands are handled before auth setup"),
        // CheckUpdate never reaches here — handled before auth setup.
		Some(Commands::CheckUpdate) => unreachable!("check-update is handled before auth setup"),
        None => {
            let download_path = get_download_path();
            let config_path = resolve_config_path("config.conf");
            let downloader = make_downloader(client.clone());

            tui::run_tui(client, downloader, download_path, config_path, log_path)?;
        }
    }

    Ok(())
}

async fn run_check_update() -> anyhow::Result<()> {
	match updater::check_now().await {
		Ok(updater::CheckOutcome::Update(info)) => {
			println!("{}", info.summary());
			println!("  {}", info.url);
			println!("  Download the zip for your platform, or `git pull && cargo build --release`.");
		}
		Ok(updater::CheckOutcome::UpToDate) => {
			println!("of-scraper-rs v{} is up to date (latest release on GitHub).", updater::CURRENT_VERSION);
		}
		Err(e) => {
			error!("Update check failed: {:#}", e);
			return Err(anyhow!("Update check failed: {:#}", e));
		}
	}
	Ok(())
}

fn run_db_command(action: DbCommand) -> anyhow::Result<()> {
    match action {
        DbCommand::Path => {
            println!("{}", db::path().display());
        },
        DbCommand::Stats { user } => {
            let stats = db::stats(user.as_deref())?;
            println!("Download database: {}", db::path().display());
            let scope = user.as_ref().map(|u| format!(" for @{}", u)).unwrap_or_default();
            if stats.total == 0 {
                println!("No recorded downloads{scope}.");
                return Ok(());
            }
            println!(
                "Total{scope}: {} (first: {}, last: {})",
                stats.total,
                stats.oldest.as_deref().map(db::fmt_timestamp).unwrap_or_default(),
                stats.newest.as_deref().map(db::fmt_timestamp).unwrap_or_default(),
            );
            for creator in stats.by_creator {
                println!("\n@{} ({}):", creator.creator, creator.total);
                println!(
                    "  Photos {}, Videos {}, Audios {}, Other {}",
                    creator.photos, creator.videos, creator.audios, creator.other
                );
                if let (Some(first), Some(last)) = (&creator.oldest, &creator.newest) {
                    println!("  first: {}   last: {}", db::fmt_timestamp(first), db::fmt_timestamp(last));
                }
            }
        },
        DbCommand::List { user, limit, offset } => {
            if let Some(user) = &user {
                println!("Recorded downloads for @{} (newest first):", user);
            } else {
                println!("Recorded downloads (newest first):");
            }
            let records = db::list(user.as_deref(), limit, offset)?;
            if records.is_empty() {
                println!("  (nothing in this page)");
                return Ok(());
            }
            for record in records {
                println!(
                    "  {:>12}  @{:<20} {:<6} {:<10} {}",
                    record.media_id,
                    record.creator,
                    record.media_type,
                    record.content_type,
                    record.downloaded_at.format("%Y-%m-%d %H:%M:%S")
                );
            }
        },
        DbCommand::Clear { user, yes } => {
            let count = db::stats(user.as_deref())?.total;
            let scope = user.as_ref().map(|u| format!(" for @{}", u)).unwrap_or_default();
            if count == 0 {
                println!("Download database is already empty{scope}.");
                return Ok(());
            }
            if !yes {
                let prompt = match &user {
                    Some(u) => format!("Delete {count} record(s) for @{}?", u),
                    None => format!("Delete ALL {count} record(s)?"),
                };
                println!("{prompt} (y/N)");
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if !answer.trim().eq_ignore_ascii_case("y") {
                    println!("Aborted; nothing deleted.");
                    return Ok(());
                }
            }
            let removed = db::clear(user.as_deref())?;
            println!("Removed {removed} record(s) from {}.", db::path().display());
        },
        DbCommand::Backfill { user, download_path } => {
            let path = download_path
                .map(PathBuf::from)
                .unwrap_or_else(get_download_path);
            println!("Backfilling from {} into {}...", path.display(), db::path().display());

            let summaries = backfill::run(user.as_deref(), &path)?;
            let mut total_seen = 0;
            let mut total_imported = 0;
            for summary in &summaries {
                println!(
                    "  @{}: {} imported, {} not matchable, {} files",
                    summary.creator, summary.imported, summary.no_match, summary.files_seen
                );
                total_seen += summary.files_seen;
                total_imported += summary.imported;
            }
            if summaries.is_empty() {
                println!("  (no creator directories found under the download path)");
            }
            println!("Done: {total_imported} imported out of {total_seen} files. Unmatchable files stay covered by the downloader's If-Modified-Since skips.");
        },
    }
    Ok(())
}
