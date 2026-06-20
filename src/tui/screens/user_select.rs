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
        match &self.inner {
            Inner::NotStarted => {
                SharedState::spawn_fetch_subscriptions(shared);
                self.inner = Inner::Loading;
            }
            Inner::Loading => {
                let state = shared.subscriptions.lock().unwrap();
                match &*state {
                    LoadState::Loaded(entries) => {
                        let fuzzy_entries: Vec<FuzzyEntry> = entries
                            .iter()
                            .map(|c| FuzzyEntry {
                                label: format!("{} (@{})", c.name, c.username),
                                value: c.username.clone(),
                            })
                            .collect();
                        drop(state);
                        self.inner = Inner::Ready(FuzzySelect::new("Pick a creator to scrape:", fuzzy_entries));
                    }
                    LoadState::Error(e) => {
                        let msg = e.clone();
                        drop(state);
                        self.inner = Inner::Error(msg);
                    }
                    LoadState::Loading => {}
                }
            }
            _ => {}
        }
        ScreenResult::Stay
    }

    fn handle_key(&mut self, key: KeyEvent, shared: &Arc<SharedState>) -> ScreenResult {
        match &mut self.inner {
            Inner::Ready(fuzzy) => match fuzzy.handle_key(key) {
                WidgetAction::Submit => {
                    if let WidgetResult::SingleSelect(username) = fuzzy.result() {
                        shared.wizard.lock().unwrap().username = Some(username);
                        return ScreenResult::Push(Box::new(
                            crate::tui::screens::content_select::ContentSelectScreen::new(),
                        ));
                    }
                    ScreenResult::Stay
                }
                WidgetAction::Cancel => ScreenResult::Pop,
                _ => ScreenResult::Stay,
            },
            _ => {
                use crossterm::event::KeyCode;
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
                let hint = Paragraph::new("  [Esc] back").style(Style::default().fg(Color::DarkGray));
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
