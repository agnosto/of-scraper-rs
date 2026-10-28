use std::{
	collections::HashMap,
	path::PathBuf,
	sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}},
};

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
    /// True while a subscriptions fetch is in flight; retry requests during
    /// one are dropped instead of piling up duplicate requests.
    subscriptions_fetching: AtomicBool,

    pub wizard: Mutex<WizardSelections>,

    /// Set once the scrape engine has actually been kicked off; the
    /// scraping screen reads/renders out of this.
    pub scrape: Mutex<Option<Arc<Mutex<ScrapeState>>>>,

    /// Same idea as `scrape`, but for an in-progress like/unlike run.
    pub like: Mutex<Option<Arc<Mutex<LikeState>>>>,

    /// Update notice once the (optional, background) check resolves.
    pub update: Mutex<Option<crate::updater::UpdateInfo>>,
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
            subscriptions_fetching: AtomicBool::new(false),
            wizard: Mutex::new(WizardSelections::default()),
            scrape: Mutex::new(None),
            like: Mutex::new(None),
            update: Mutex::new(None),
        }
    }

    /// (Re-)start the subscriptions fetch: flips the shared cache back to
    /// `Loading` — so a screen that just hit an old error shows a spinner,
    /// not the stale error — and fires the request in the background. No-op
    /// while a fetch is already in flight (pressing retry repeatedly can't
    /// stack up duplicate bursts, which would rate-limit harder).
    pub fn spawn_fetch_subscriptions(self_arc: &Arc<SharedState>) {
        if self_arc.subscriptions_fetching.swap(true, Ordering::SeqCst) {
            return;
        }
        *self_arc.subscriptions.lock().unwrap() = LoadState::Loading;
        let shared = self_arc.clone();
        tokio::spawn(async move {
            let cfg = crate::config::config();
            let result = shared
                .client
                .get_subscriptions_filtered(
                    cfg.subscriptions.include_expired_subscriptions,
                    cfg.subscriptions.include_restricted_subscriptions,
                )
                .await;
            // Clear the in-flight guard first so a retry pressed while the
            // slot is being written isn't lost.
            shared.subscriptions_fetching.store(false, Ordering::SeqCst);
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
