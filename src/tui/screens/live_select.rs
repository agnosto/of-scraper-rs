use std::collections::HashMap;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::tui::{
    app::{RecordingState, SharedState},
    screens::{Screen, ScreenResult},
    types::{LoadState, WidgetAction, WidgetResult},
    widgets::fuzzy_select::{FuzzyEntry, FuzzySelect},
};

enum Inner {
    /// Subscriptions haven't been requested yet (set on first tick).
    NotStarted,
    Loading,
    Error(String),
    Ready(FuzzySelect),
}

/// Creator picker for livestream recording: the subscription list with a
/// LIVE badge on broadcasting creators (live first). Picking a live creator
/// starts the recording; picking one who isn't live just says so.
pub struct LiveSelectScreen {
    inner: Inner,
    /// username -> (user id, is live), rebuilt with the picker.
    creators: HashMap<String, (u64, bool)>,
    /// One-shot message shown under the list (e.g. "not live right now").
    notice: Option<String>,
}

impl LiveSelectScreen {
    pub fn new() -> Self {
        Self { inner: Inner::NotStarted, creators: HashMap::new(), notice: None }
    }

    fn start_recording(shared: &Arc<SharedState>, username: String, user_id: u64) {
        let state = Arc::new(std::sync::Mutex::new(RecordingState::new(username, user_id)));
        *shared.recording.lock().unwrap() = Some(state.clone());
        let client = shared.client.clone();
        let download_path = shared.download_path.clone();
        tokio::spawn(async move {
            crate::run_recording_tui(&client, state, &download_path).await;
        });
    }
}

impl Screen for LiveSelectScreen {
    fn tick(&mut self, shared: &Arc<SharedState>) -> ScreenResult {
        if matches!(self.inner, Inner::NotStarted) {
            SharedState::spawn_fetch_subscriptions(shared);
            self.inner = Inner::Loading;
            return ScreenResult::Stay;
        }
        if !matches!(self.inner, Inner::Loading | Inner::Error(_)) {
            return ScreenResult::Stay;
        }
        match &*shared.subscriptions.lock().unwrap() {
            LoadState::Loading => {
                self.inner = Inner::Loading;
            }
            LoadState::Error(e) => {
                self.inner = Inner::Error(e.clone());
            }
            LoadState::Loaded(entries) => {
                // Live creators first, then alphabetical — the point of the
                // screen is recording someone who is on right now.
                let mut sorted = entries.clone();
                sorted.sort_by(|a, b| {
                    b.is_live.cmp(&a.is_live).then_with(|| a.username.to_lowercase().cmp(&b.username.to_lowercase()))
                });
                self.creators = sorted
                    .iter()
                    .map(|c| (c.username.clone(), (c.id, c.is_live)))
                    .collect();
                let fuzzy_entries: Vec<FuzzyEntry> = sorted
                    .iter()
                    .map(|c| {
                        let badge = if c.is_live { " ● LIVE" } else { "" };
                        FuzzyEntry {
                            label: format!("{} (@{}){}", c.name, c.username, badge),
                            value: c.username.clone(),
                        }
                    })
                    .collect();
                let live_count = sorted.iter().filter(|c| c.is_live).count();
                let title = format!(
                    "Record a livestream — {} of {} creators live now. [R] refresh  [Esc] back",
                    live_count,
                    sorted.len()
                );
                self.inner = Inner::Ready(FuzzySelect::new(title, fuzzy_entries));
            }
        }
        ScreenResult::Stay
    }

    fn handle_key(&mut self, key: KeyEvent, shared: &Arc<SharedState>) -> ScreenResult {
        match &mut self.inner {
            Inner::Ready(fuzzy) => match fuzzy.handle_key(key) {
                WidgetAction::Submit => {
                    if let WidgetResult::SingleSelect(username) = fuzzy.result()
                        && let Some((user_id, is_live)) = self.creators.get(&username).copied()
                    {
                        if is_live {
                            Self::start_recording(shared, username, user_id);
                            return ScreenResult::Push(Box::new(
                                crate::tui::screens::recording::RecordingScreen::new(),
                            ));
                        }
                        self.notice = Some(format!("@{username} is not broadcasting right now — [R] to refresh"));
                    }
                    ScreenResult::Stay
                }
                WidgetAction::Cancel => ScreenResult::Pop,
                _ => {
                    // [R] re-checks who is live by refetching subscriptions.
                    if matches!(key.code, KeyCode::Char('r') | KeyCode::Char('R')) {
                        SharedState::spawn_fetch_subscriptions(shared);
                        self.inner = Inner::Loading;
                        self.notice = Some("Refreshing live status...".to_string());
                    }
                    ScreenResult::Stay
                }
            },
            Inner::Error(_) => match key.code {
                KeyCode::Char('r') | KeyCode::Char('R') | KeyCode::Enter => {
                    SharedState::spawn_fetch_subscriptions(shared);
                    ScreenResult::Stay
                }
                KeyCode::Esc => ScreenResult::Pop,
                _ => ScreenResult::Stay,
            },
            _ => {
                if key.code == KeyCode::Esc {
                    ScreenResult::Pop
                } else if matches!(key.code, KeyCode::Char('r') | KeyCode::Char('R')) {
                    SharedState::spawn_fetch_subscriptions(shared);
                    self.inner = Inner::Loading;
                    ScreenResult::Stay
                } else {
                    ScreenResult::Stay
                }
            }
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _shared: &Arc<SharedState>) {
        match &mut self.inner {
            Inner::Ready(fuzzy) => {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(3), Constraint::Length(if self.notice.is_some() { 1 } else { 0 })])
                    .split(area);
                fuzzy.render(frame, chunks[0]);
                if let Some(notice) = &self.notice {
                    let p = Paragraph::new(notice.as_str())
                        .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::ITALIC));
                    frame.render_widget(p, chunks[1]);
                }
            }
            Inner::Error(msg) => {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(3), Constraint::Length(1)])
                    .split(area);
                let p = Paragraph::new(msg.as_str())
                    .style(Style::default().fg(Color::Red))
                    .block(Block::default().borders(Borders::ALL).title(" Error fetching subscriptions "));
                frame.render_widget(p, chunks[0]);
                let hint = Paragraph::new("  [R] retry  [Esc] back").style(Style::default().fg(Color::DarkGray));
                frame.render_widget(hint, chunks[1]);
            }
            _ => {
                let p = Paragraph::new("Loading subscriptions...")
                    .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::ITALIC));
                frame.render_widget(p, area);
            }
        }
    }
}
