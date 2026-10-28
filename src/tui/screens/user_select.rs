use std::sync::Arc;

use crossterm::event::KeyEvent;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::tui::{
    app::SharedState,
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

pub struct UserSelectScreen {
    inner: Inner,
}

impl UserSelectScreen {
    pub fn new() -> Self {
        Self { inner: Inner::NotStarted }
    }
}

impl Screen for UserSelectScreen {
    fn tick(&mut self, shared: &Arc<SharedState>) -> ScreenResult {
        // Fresh screen: kick a fetch off and wait for its current result.
        // NotStarted never falls back to the slot read below, so entering
        // the screen after an error shows a spinner for the new request
        // instead of flashing the stale error for a frame.
        if matches!(self.inner, Inner::NotStarted) {
            SharedState::spawn_fetch_subscriptions(shared);
            self.inner = Inner::Loading;
            return ScreenResult::Stay;
        }

        // From here on the screen mirrors the shared cache: Loading stays a
        // spinner, Error shows the message, Loaded builds the picker. A
        // retry running while the error screen is up flips it to the
        // spinner; a success flips it straight into the picker, without a
        // manual back-and-forth from the user.
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
                let fuzzy_entries: Vec<FuzzyEntry> = entries
                    .iter()
                    .map(|c| FuzzyEntry {
                        label: format!("{} (@{})", c.name, c.username),
                        value: c.username.clone(),
                    })
                    .collect();
                self.inner = Inner::Ready(FuzzySelect::new("Pick a creator to scrape:", fuzzy_entries));
            }
        }
        ScreenResult::Stay
    }

    fn handle_key(&mut self, key: KeyEvent, shared: &Arc<SharedState>) -> ScreenResult {
        use crossterm::event::KeyCode;

        match &mut self.inner {
            Inner::Ready(fuzzy) => match fuzzy.handle_key(key) {
                WidgetAction::Submit => {
                    if let WidgetResult::SingleSelect(username) = fuzzy.result() {
                        shared.wizard.lock().unwrap().username = Some(username);
                        let mode = shared.wizard.lock().unwrap().mode;
                        return match mode {
                            crate::tui::app::WizardMode::Scrape => ScreenResult::Push(Box::new(
                                crate::tui::screens::content_select::ContentSelectScreen::new(),
                            )),
                            crate::tui::app::WizardMode::Like => ScreenResult::Push(Box::new(
                                crate::tui::screens::like_options::LikeOptionsScreen::new(),
                            )),
                        };
                    }
                    ScreenResult::Stay
                }
                WidgetAction::Cancel => ScreenResult::Pop,
                _ => ScreenResult::Stay,
            },
            Inner::Error(_) => match key.code {
                KeyCode::Char('r') | KeyCode::Char('R') | KeyCode::Enter => {
                    // No-op while the previous fetch is still in flight.
                    SharedState::spawn_fetch_subscriptions(shared);
                    ScreenResult::Stay
                }
                KeyCode::Esc => ScreenResult::Pop,
                _ => ScreenResult::Stay,
            },
            _ => {
                if key.code == KeyCode::Esc {
                    ScreenResult::Pop
                } else {
                    ScreenResult::Stay
                }
            }
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _shared: &Arc<SharedState>) {
        match &mut self.inner {
            Inner::Ready(fuzzy) => fuzzy.render(frame, area),
            Inner::Error(msg) => {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(3), Constraint::Length(1)])
                    .split(area);
                let p = Paragraph::new(msg.as_str())
                    .style(Style::default().fg(Color::Red))
                    .block(Block::default().borders(Borders::ALL).title(" Error fetching subscriptions "));
                frame.render_widget(p, chunks[0]);
                let hint = if msg.contains("429") {
                    Paragraph::new("  OnlyFans rate limited the request — the retry waits it out. [R] retry again  [Esc] back")
                        .style(Style::default().fg(Color::Yellow))
                } else {
                    Paragraph::new("  [R] retry  [Esc] back")
                        .style(Style::default().fg(Color::DarkGray))
                };
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
