use std::{collections::HashMap, path::PathBuf, sync::{Arc, Mutex}};

use of_client::OFClient;

use crate::downloader::Downloader;
use crate::tui::types::LoadState;

/// One in-flight download, shown in the "Active Downloads" pane.
pub struct ActiveDownload {
    pub filename: String,
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
}

/// Live progress/log state for an in-progress scrape. This is what the
/// scrape engine in `main.rs` mutates as it runs.
pub struct ScrapeState {
    pub username: String,
    pub status: String,
    pub posts_scraped: usize,
    pub chats_scraped: usize,
    pub stories_scraped: usize,
    pub files_downloaded: usize,
    pub files_failed: usize,
    pub active_downloads: HashMap<u64, ActiveDownload>,
    pub logs: Vec<String>,
    pub download_path: PathBuf,
    pub is_finished: bool,
    pub should_quit: bool,
}

impl ScrapeState {
    pub fn new(username: String, download_path: PathBuf) -> Self {
        Self {
            username,
            status: "Waiting to start...".to_string(),
            posts_scraped: 0,
            chats_scraped: 0,
            stories_scraped: 0,
            files_downloaded: 0,
            files_failed: 0,
            active_downloads: HashMap::new(),
            logs: Vec::new(),
            download_path,
            is_finished: false,
            should_quit: false,
        }
    }

    pub fn log(&mut self, msg: String) {
        if self.logs.len() > 100 {
            self.logs.remove(0);
        }
        self.logs.push(msg);
    }
}

pub fn log_message(state: &Option<Arc<Mutex<ScrapeState>>>, msg: &str) {
    if let Some(state) = state {
        state.lock().unwrap().log(msg.to_string());
    } else {
        println!("{}", msg);
    }
}

/// A creator pulled from `client.get_subscriptions()`, trimmed down to
/// what the picker screen needs.
#[derive(Clone, Debug)]
pub struct CreatorEntry {
    pub name: String,
    pub username: String,
}

/// Selections the wizard screens (user pick -> content-type pick) build up
/// before the scrape engine is actually launched.
#[derive(Default)]
pub struct WizardSelections {
    pub username: Option<String>,
    pub content_types: Vec<String>,
}

/// Everything screens need access to. One instance lives for the lifetime
/// of the TUI and is threaded through `Screen::handle_key` / `render`.
pub struct SharedState {
    pub client: OFClient,
    pub downloader: Arc<Downloader>,
    pub download_path: PathBuf,
    pub config_path: PathBuf,

    /// Cached subscriptions list, fetched once and reused by the picker
    /// screen (and any future "pick a creator" screen).
    pub subscriptions: Mutex<LoadState<Vec<CreatorEntry>>>,

    pub wizard: Mutex<WizardSelections>,

    /// Set once the scrape engine has actually been kicked off; the
    /// scraping screen reads/renders out of this.
    pub scrape: Mutex<Option<Arc<Mutex<ScrapeState>>>>,

    pub should_quit: Mutex<bool>,
}

impl SharedState {
    pub fn new(client: OFClient, downloader: Arc<Downloader>, download_path: PathBuf, config_path: PathBuf) -> Self {
        Self {
            client,
            downloader,
            download_path,
            config_path,
            subscriptions: Mutex::new(LoadState::Loading),
            wizard: Mutex::new(WizardSelections::default()),
            scrape: Mutex::new(None),
            should_quit: Mutex::new(false),
        }
    }

    /// Kick off `client.get_subscriptions()` in the background exactly
    /// once; safe to call repeatedly (only fires the request if we're
    /// still in `Loading` and haven't started yet — callers should guard
    /// with their own "already requested" flag if they call this on every
    /// frame; here we just overwrite, so call it once on screen entry).
    pub fn spawn_fetch_subscriptions(self_arc: &Arc<SharedState>) {
        let shared = self_arc.clone();
        tokio::spawn(async move {
            let result = shared.client.get_subscriptions().await;
            let mut slot = shared.subscriptions.lock().unwrap();
            match result {
                Ok(subs) => {
                    let entries = subs
                        .into_iter()
                        .map(|s| CreatorEntry { name: s.name, username: s.username })
                        .collect();
                    *slot = LoadState::Loaded(entries);
                }
                Err(e) => {
                    *slot = LoadState::Error(format!("Failed to load subscriptions: {}", e));
                }
            }
        });
    }
}
