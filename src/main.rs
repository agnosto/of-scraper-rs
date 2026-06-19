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

use tui::{AppState, ActiveDownload, log_message, run_tui};

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
        user: String,
        /// Type of content to scrape (posts, chats, stories, all)
        #[arg(short, long, default_value = "all")]
        content_type: String,
    },
    /// List subscriptions
    List,
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
    pub fn load(filename: &str) -> std::io::Result<Self> {
        let mut values = HashMap::new();
        let content = fs::read_to_string(filename)?;
        let mut current_block: Vec<String> = Vec::new();
        
        for line in content.lines() {
            let mut line = line.trim();
            if line.is_empty() {
                continue;
            }
            
            // Strip comments
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

fn load_auth() -> anyhow::Result<AuthParams> {
    if Path::new("auth.json").exists() {
        return load_auth_json();
    } else if Path::new("config.conf").exists() {
        return load_config_conf();
    }
    Err(anyhow!("Neither auth.json nor config.conf found"))
}

fn load_auth_json() -> anyhow::Result<AuthParams> {
    #[derive(serde::Deserialize)]
    struct AuthFileInner {
        cookie: String,
        user_agent: String,
        x_bc: String,
    }
    #[derive(serde::Deserialize)]
    struct AuthFile { auth: AuthFileInner }

    let data = fs::read_to_string("auth.json")?;
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

fn load_config_conf() -> anyhow::Result<AuthParams> {
    let config = CustomConfig::load("config.conf").map_err(|e| anyhow!("Config load error: {}", e))?;

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
    if Path::new("device.wvd").exists() {
        match fs::File::open("device.wvd") {
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
    if let Ok(config) = CustomConfig::load("config.conf") {
        if let Some(p) = config.get("download", "downloadpath") {
            if !p.is_empty() {
                path = PathBuf::from(p);
            }
        }
    }
    path
}

async fn queue_downloads<T>(
    downloader: &Arc<downloader::Downloader>,
    semaphore: &Arc<tokio::sync::Semaphore>,
    state_opt: &Option<Arc<Mutex<AppState>>>,
    content: &T,
    user: &of_client::user::User,
    download_path: &Path,
) where
    T: of_client::content::Content + of_client::content::HasMedia<Media = of_client::media::Feed> + Send + Sync + 'static,
{
    use of_client::content::ContentType;
    use of_client::media::MediaType;
    use of_client::media::Media;
    
    let content_type_str = T::content_type().to_string();
    let content_path = download_path.join(&user.username).join(&content_type_str);
    
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
        let content_type = T::content_type();
        
        tokio::spawn(async move {
            let _permit = match semaphore.acquire().await {
                Ok(p) => p,
                Err(_) => return,
            };
            
            let filename = if let Some(url_str) = media.source() {
                url_str.split('/').last().unwrap_or("file").to_string()
            } else if let Some(_drm) = media.drm() {
                format!("{}.mp4", media_id)
            } else {
                format!("{}", media_id)
            };
            
            log_message(&state_opt, &format!("Starting download of {}", filename));
            
            let res = if is_drm {
                let type_str = match content_type {
                    ContentType::Chats => "message",
                    _ => "post",
                };
                let license_url = format!(
                    "https://onlyfans.com/api2/v2/users/media/{}/drm/{}/{}?type=widevine",
                    media_id, type_str, content_id
                );
                if let Some(drm) = media.drm() {
                    downloader.download_media_drm(drm, &license_url, &path, media_id).await
                } else {
                    Err(anyhow!("DRM manifest missing"))
                }
            } else {
                downloader.download_media(&media, &path).await
            };
            
            match res {
                Ok(_) => log_message(&state_opt, &format!("Successfully downloaded {}", filename)),
                Err(e) => log_message(&state_opt, &format!("Failed to download {}: {}", filename, e)),
            }
        });
    }
}

pub async fn run_scrape_engine_tui(
    client: OFClient,
    downloader: Arc<downloader::Downloader>,
    username: String,
    content_types: Vec<String>,
    state: Arc<Mutex<AppState>>,
) -> anyhow::Result<()> {
    {
        let mut s = state.lock().unwrap();
        s.username = username.clone();
    }
    log_message(&Some(state.clone()), &format!("Fetching user @{}...", username));
    let user = match client.get_user(username.as_str()).await {
        Ok(u) => u,
        Err(e) => {
            let err_msg = format!("Failed to get user @{}: {}", username, e);
            log_message(&Some(state.clone()), &err_msg);
            state.lock().unwrap().status = "Failed".to_string();
            state.lock().unwrap().is_finished = true;
            return Err(anyhow!(err_msg));
        }
    };
    
    log_message(&Some(state.clone()), &format!("User found: {} (ID: {})", user.name, user.id));
    
    let download_path = get_download_path();
    let semaphore = Arc::new(tokio::sync::Semaphore::new(4));
    
    for t in &content_types {
        if state.lock().unwrap().should_quit { break; }
        
        match t.as_str() {
            "posts" => {
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
                        
                        queue_downloads(&downloader, &semaphore, &Some(state.clone()), post, &user, &download_path).await;
                        
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
                        
                        queue_downloads(&downloader, &semaphore, &Some(state.clone()), chat, &user, &download_path).await;
                        
                        before_id = Some(chat.id);
                        count += 1;
                    }
                    
                    if count < 10 { break; }
                }
            }
            "stories" => {
                if state.lock().unwrap().should_quit { break; }
                
                state.lock().unwrap().status = "Scraping stories...".to_string();
                match client.get_stories(user.id).await {
                    Ok(stories) => {
                        for story in &stories {
                            if state.lock().unwrap().should_quit { break; }
                            
                            state.lock().unwrap().stories_scraped += 1;
                            
                            queue_downloads(&downloader, &semaphore, &Some(state.clone()), story, &user, &download_path).await;
                        }
                    }
                    Err(e) => {
                        log_message(&Some(state.clone()), &format!("Failed to get stories: {}", e));
                    }
                }
            }
            _ => log_message(&Some(state.clone()), &format!("Content type '{}' not recognized", t)),
        }
    }
    
    state.lock().unwrap().status = "Finishing downloads...".to_string();
    log_message(&Some(state.clone()), "Scraping loop finished. Waiting for active downloads to complete...");
    
    loop {
        let is_empty = state.lock().unwrap().active_downloads.is_empty();
        if is_empty || state.lock().unwrap().should_quit {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
    
    state.lock().unwrap().is_finished = true;
    log_message(&Some(state.clone()), "All operations finished.");
    Ok(())
}

async fn scrape_user_cli(
    client: &OFClient,
    downloader: Arc<downloader::Downloader>,
    username: &str,
    content_type: &str,
) -> anyhow::Result<()> {
    println!("Scraping @{} for {}...", username, content_type);
    let user = client.get_user(username).await.map_err(|e| anyhow!("Failed to get user: {}", e))?;
    println!("User found: {} ({})", user.name, user.id);
    
    let download_path = get_download_path();
    let semaphore = Arc::new(tokio::sync::Semaphore::new(4));
    
    let types = match content_type {
        "all" => vec!["posts", "chats", "stories"],
        other => vec![other],
    };
    
    for t in types {
        match t {
            "posts" => {
                let mut before: Option<DateTime<Utc>> = None;
                loop {
                    let posts = client.get_posts(user.id, before).await.map_err(|e| anyhow!("Failed to get posts: {}", e))?;
                    if posts.is_empty() { break; }
                    let mut count = 0;
                    for post in &posts {
                        println!("Queueing post media from post {}", post.id);
                        queue_downloads(&downloader, &semaphore, &None, post, &user, &download_path).await;
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
                        println!("Queueing chat media from message {}", chat.id);
                        queue_downloads(&downloader, &semaphore, &None, chat, &user, &download_path).await;
                        before_id = Some(chat.id);
                        count += 1;
                    }
                    if count < 10 { break; }
                }
            }
            "stories" => {
                println!("Scraping active stories...");
                match client.get_stories(user.id).await {
                    Ok(stories) => {
                        for story in &stories {
                            queue_downloads(&downloader, &semaphore, &None, story, &user, &download_path).await;
                        }
                    }
                    Err(e) => println!("Failed to get stories: {}", e),
                }
            }
            _ => println!("Content type '{}' not recognized", t),
        }
    }
    
    println!("Waiting for active downloads to complete...");
    let _permits = semaphore.acquire_many(4).await?;
    println!("Scraping and downloads completed successfully.");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let auth = load_auth().context("Loading authentication")?;
    let client = OFClient::new(auth)?;
    
    match cli.command {
        Some(Commands::Scrape { user, content_type }) => {
            let cdm = load_cdm();
            let downloader = Arc::new(downloader::Downloader::new(client.clone(), cdm, None));
            scrape_user_cli(&client, downloader, &user, &content_type).await?;
        },
        Some(Commands::List) => {
            let subs = client.get_subscriptions().await.map_err(|e| anyhow!("Failed to get subscriptions: {}", e))?;
            for sub in subs {
                println!("- {} (@{})", sub.name, sub.username);
            }
        },
        None => {
            let download_path = get_download_path();
            let state = Arc::new(Mutex::new(AppState::new("".to_string(), download_path)));
            
            let cdm = load_cdm();
            let downloader = Arc::new(downloader::Downloader::new(client.clone(), cdm, Some(state.clone())));
            
            run_tui(state, client, downloader)?;
        }
    }
    
    Ok(())
}
