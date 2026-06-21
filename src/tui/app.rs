use std::{collections::HashMap, path::PathBuf, sync::{Arc, Mutex}};

use log::info;
use of_client::OFClient;

use crate::downloader::Downloader;
use crate::tui::types::LoadState;

/// One in-flight download, shown in the "Active Downloads" pane.
pub struct ActiveDownload {
    pub filename: String,
    pub creator: String,
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
    pub purchases_scraped: usize,
    pub highlights_scraped: usize,
    pub files_downloaded: usize,
    pub files_failed: usize,
    pub active_downloads: HashMap<u64, ActiveDownload>,
    /// Incremented synchronously the moment a download task is queued
    /// (before `tokio::spawn` even returns) and decremented when that task
    /// finishes. `active_downloads` alone isn't enough to know when
    /// everything is done — a just-spawned task hasn't inserted itself
    /// into that map yet, so checking only `active_downloads.is_empty()`
    /// can report "done" while downloads are still about to start.
    pub pending_downloads: usize,
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
            purchases_scraped: 0,
            highlights_scraped: 0,
            files_downloaded: 0,
            files_failed: 0,
            active_downloads: HashMap::new(),
            pending_downloads: 0,
            logs: Vec::new(),
            download_path,
            is_finished: false,
            should_quit: false,
        }
    }

    pub fn log(&mut self, msg: String) {
        info!("{}", msg);
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

/// Live progress/log state for an in-progress like/unlike run. Deliberately
/// separate from `ScrapeState` rather than reusing it — a like run has no
/// downloads, no `active_downloads`/`pending_downloads`, and tracks
/// "liked"/"unliked"/"skipped" counts instead of files.
pub struct LikeState {
    pub username: String,
    pub status: String,
    pub liking: bool,
    pub posts_processed: usize,
    pub chats_processed: usize,
    pub stories_processed: usize,
    pub changed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub logs: Vec<String>,
    pub is_finished: bool,
    pub should_quit: bool,
}

impl LikeState {
    pub fn new(username: String, liking: bool) -> Self {
        Self {
            username,
            status: "Waiting to start...".to_string(),
            liking,
            posts_processed: 0,
            chats_processed: 0,
            stories_processed: 0,
            changed: 0,
            skipped: 0,
            failed: 0,
            logs: Vec::new(),
            is_finished: false,
            should_quit: false,
        }
    }

    pub fn log(&mut self, msg: String) {
        info!("{}", msg);
        if self.logs.len() > 100 {
            self.logs.remove(0);
        }
        self.logs.push(msg);
    }
}

pub fn log_like_message(state: &Arc<Mutex<LikeState>>, msg: &str) {
    state.lock().unwrap().log(msg.to_string());
}

/// A creator pulled from `client.get_subscriptions()`, trimmed down to
/// what the picker screen needs.
#[derive(Clone, Debug)]
pub struct CreatorEntry {
    pub name: String,
    pub username: String,
}

/// Which flow the creator picker should hand off to once a creator is
/// chosen — added so `UserSelectScreen` can be shared between the scrape
/// wizard and the like/unlike wizard instead of duplicating the
/// fuzzy-picker screen for each.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum WizardMode {
    #[default]
    Scrape,
    Like,
}

/// Selections the wizard screens (user pick -> content-type pick) build up
/// before the scrape engine is actually launched.
#[derive(Default)]
pub struct WizardSelections {
    pub mode: WizardMode,
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
    pub log_path: PathBuf,

    /// Cached subscriptions list, fetched once and reused by the picker
    /// screen (and any future "pick a creator" screen).
    pub subscriptions: Mutex<LoadState<Vec<CreatorEntry>>>,

    pub wizard: Mutex<WizardSelections>,

    /// Set once the scrape engine has actually been kicked off; the
    /// scraping screen reads/renders out of this.
    pub scrape: Mutex<Option<Arc<Mutex<ScrapeState>>>>,

    /// Same idea as `scrape`, but for an in-progress like/unlike run.
    pub like: Mutex<Option<Arc<Mutex<LikeState>>>>,

    pub should_quit: Mutex<bool>,
}

impl SharedState {
    pub fn new(client: OFClient, downloader: Arc<Downloader>, download_path: PathBuf, config_path: PathBuf, log_path: PathBuf) -> Self {
        Self {
            client,
            downloader,
            download_path,
            config_path,
            log_path,
            subscriptions: Mutex::new(LoadState::Loading),
            wizard: Mutex::new(WizardSelections::default()),
            scrape: Mutex::new(None),
            like: Mutex::new(None),
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
