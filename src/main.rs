use std::{fs, sync::{Arc, Mutex}, path::{Path, PathBuf}, collections::HashMap};
use of_client::{reqwest_cookie_store::{CookieStore, CookieStoreRwLock}, OFClient, RequestHeaders};
use reqwest::Url;
use cookie::Cookie;
use anyhow::{anyhow, Context};
use log::*;
use clap::{Parser, Subcommand};
use chrono::{DateTime, Utc};
use dirs;

mod tui;
mod downloader;

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

            if line.ends_with('{') {
                let block_name = line[..line.len() - 1].trim().to_lowercase();
                current_block.push(block_name);
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

    simplelog::WriteLogger::init(simplelog::LevelFilter::Debug, simplelog::Config::default(), file)
        .map_err(|e| anyhow!("Failed to initialize logger: {e}"))?;

    info!("=== of-scraper-rs v{} starting ===", env!("CARGO_PKG_VERSION"));
    Ok(log_path)
}

fn resolve_config_path(filename: &str) -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(filename);
            if candidate.exists() {
                return candidate;
            }
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
    let mut path = PathBuf::from("data");
    let config_conf = resolve_config_path("config.conf");
    if config_conf.exists() {
        if let Ok(config) = CustomConfig::load(&config_conf) {
            if let Some(p) = config.get("download", "downloadpath") {
                if !p.is_empty() {
                    path = PathBuf::from(p);
                }
            }
        }
    }
    path
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
    use of_client::media::MediaType;
    use of_client::media::Media;

    let content_type_str = T::content_type().to_string();
    let mut content_path = download_path.join(username).join(&content_type_str);
    if let Some(sub) = subfolder {
        // Label/highlight titles can contain characters that are awkward
        // in paths (slashes, etc); keep it simple and just strip those.
        let sanitized: String = sub.chars().filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')).collect();
        content_path = content_path.join(sanitized);
    }

    let mut handles = Vec::new();

    for media in content.media().iter().cloned() {
        let downloader = downloader.clone();
        let semaphore = semaphore.clone();
        let state_opt = state_opt.clone();
        let path = content_path.join(match media.media_type() {
            MediaType::Photo => "Images",
            MediaType::Audio => "Audios",
            MediaType::Video | MediaType::Gif => "Videos",
        });

        let is_drm = media.drm().is_some();
        let content_id = content.id();
        let media_id = media.id;
        let drm_type_str = content.drm_type_str();

        if !is_drm && media.source().is_none() {
            let msg = format!(
                "Skipping media {} from content {} ({}): no source URL and not DRM — likely still locked despite appearing in this listing",
                media_id, content_id, content_type_str
            );
            warn!("{}", msg);
            log_message(&state_opt, &msg);
            continue;
        }

        if let Some(state) = &state_opt {
            state.lock().unwrap().pending_downloads += 1;
        }

        let creator_name = username.to_string();

        let handle = tokio::spawn(async move {
            let _permit = match semaphore.acquire().await {
                Ok(p) => p,
                Err(_) => {
                    if let Some(state) = &state_opt {
                        state.lock().unwrap().pending_downloads -= 1;
                    }
                    return;
                }
            };

            let filename = if let Some(url_str) = media.source() {
                //url_str.split('/').last().unwrap_or("file").to_string()
                if let Ok(url) = Url::parse(url_str) {
                    url.path_segments()
                        .and_then(|s| s.last())
                        .unwrap_or("file")
                        .to_string()
                } else {
                    "file".to_string()
                }
            } else if let Some(_drm) = media.drm() {
                format!("{}.mp4", media_id)
            } else {
                format!("{}", media_id)
            };

            log_message(&state_opt, &format!("Starting download of {}", filename));

            let res = if is_drm {
                let license_url = format!(
                    "https://onlyfans.com/api2/v2/users/media/{}/drm/{}/{}?type=widevine",
                    media_id, drm_type_str, content_id
                );
                if let Some(drm) = media.drm() {
                    downloader.download_media_drm_tracked(drm, &license_url, &path, media_id, state_opt.clone(), creator_name).await
                } else {
                    Err(anyhow!("DRM manifest missing"))
                }
            } else {
                downloader.download_media_tracked(&media, &path, state_opt.clone(), creator_name).await
            };

            match res {
                Ok(_) => log_message(&state_opt, &format!("Successfully downloaded {}", filename)),
                Err(e) => log_message(&state_opt, &format!("Failed to download {}: {}", filename, e)),
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

                        state.lock().unwrap().posts_scraped += 1;

                        queue_downloads(&downloader, &semaphore, &Some(state.clone()), post, &user.username, &download_path, None).await;

                        before = Some(post.posted_at);
                        count += 1;
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

                        state.lock().unwrap().chats_scraped += 1;

                        queue_downloads(&downloader, &semaphore, &Some(state.clone()), chat, &user.username, &download_path, None).await;

                        before_id = Some(chat.id);
                        count += 1;
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

                                state.lock().unwrap().posts_scraped += 1;

                                queue_downloads(&downloader, &semaphore, &Some(state.clone()), post, &user.username, &download_path, Some(&label.name)).await;

                                before = Some(post.posted_at);
                                count += 1;
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

    loop {
        let nothing_pending = state.lock().unwrap().pending_downloads == 0;
        if nothing_pending || state.lock().unwrap().should_quit {
            break;
        }
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
    loop {
        let nothing_pending = state.lock().unwrap().pending_downloads == 0;
        if nothing_pending || state.lock().unwrap().should_quit { break; }
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
    use of_client::content::CanLike;

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

    if let Some(pos) = segments.iter().position(|s| *s == "chat") {
        if let Some(user_id_str) = segments.get(pos + 1) {
            if let Ok(user_id) = user_id_str.parse::<u64>() {
                let message_id: u64 = url.query_pairs()
                    .find(|(k, _)| k == "firstId")
                    .map(|(_, v)| v.into_owned())
                    .ok_or_else(|| anyhow!("Chat link is missing '?firstId=' — needed to identify the message"))?
                    .parse()
                    .map_err(|_| anyhow!("Couldn't parse the message id out of '?firstId='"))?;
                return Ok(LinkTarget::Message { user_id, message_id });
            }
        }
    }

    // Post link: onlyfans.com/{postId}/{username}
    if let Some(first) = segments.first() {
        if let Ok(post_id) = first.parse::<u64>() {
            return Ok(LinkTarget::Post { post_id });
        }
    }

    Err(anyhow!(
        "Couldn't recognize '{}' as a post link (onlyfans.com/{{postId}}/{{username}}) \
         or a message link (onlyfans.com/my/chats/chat/{{userId}}/?firstId={{messageId}})",
        input
    ))
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
    use of_client::content::CanLike;

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
        match t {
            "posts" => {
                let Some(user) = user.as_ref() else { println!("Skipping posts: user could not be resolved"); continue };
                let mut before: Option<DateTime<Utc>> = None;
                loop {
                    let posts = client.get_posts(user.id, before).await.map_err(|e| anyhow!("Failed to get posts: {}", e))?;
                    if posts.is_empty() { break; }
                    let mut count = 0;
                    for post in &posts {
                        println!("Queueing post media from post {}", post.id);
                        tasks.extend(queue_downloads(&downloader, &semaphore, &None, post, &user.username, &download_path, None).await);
                        before = Some(post.posted_at);
                        count += 1;
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
                        println!("Queueing chat media from message {}", chat.id);
                        tasks.extend(queue_downloads(&downloader, &semaphore, &None, chat, &user.username, &download_path, None).await);
                        before_id = Some(chat.id);
                        count += 1;
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
                        let mut before: Option<DateTime<Utc>> = None;
                        loop {
                            let posts = client.get_posts_by_label(user.id, &label.id, before).await.map_err(|e| anyhow!("Failed to get posts for label '{}': {}", label.name, e))?;
                            if posts.is_empty() { break; }
                            let mut count = 0;
                            for post in &posts {
                                println!("Queueing post {} from label '{}'", post.id, label.name);
                                tasks.extend(queue_downloads(&downloader, &semaphore, &None, post, &user.username, &download_path, Some(&label.name)).await);
                                before = Some(post.posted_at);
                                count += 1;
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
    let log_path = init_file_logging().context("Setting up logging")?;

    let cli = Cli::parse();

    let auth = match load_auth().context("Loading authentication") {
        Ok(a) => a,
        Err(e) => {
            error!("Failed to load auth: {:#}", e);
            return Err(e);
        }
    };
    let client = OFClient::new(auth)?;

    match cli.command {
        Some(Commands::Scrape { user, content_type }) => {
            let cdm = load_cdm();
            let downloader = Arc::new(downloader::Downloader::new(client.clone(), cdm));
            scrape_user_cli(&client, downloader, &user, &content_type).await?;
        },
        Some(Commands::List) => {
            let subs = client.get_subscriptions().await.map_err(|e| anyhow!("Failed to get subscriptions: {}", e))?;
            for sub in subs {
                println!("- {} (@{})", sub.name, sub.username);
            }
        },
        Some(Commands::Like { user, content_type, unlike }) => {
            like_user_cli(&client, &user, &content_type, !unlike).await?;
        },
        Some(Commands::Link { url }) => {
            let cdm = load_cdm();
            let downloader = Arc::new(downloader::Downloader::new(client.clone(), cdm));
            download_link_cli(&client, downloader, &url).await?;
        },
        None => {
            let download_path = get_download_path();
            let config_path = resolve_config_path("config.conf");
            let cdm = load_cdm();
            let downloader = Arc::new(downloader::Downloader::new(client.clone(), cdm));

            tui::run_tui(client, downloader, download_path, config_path, log_path)?;
        }
    }

    Ok(())
}
