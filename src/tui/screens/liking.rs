use std::sync::Arc;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};

use crate::tui::{app::SharedState, screens::{Screen, ScreenResult}};

pub struct LikingScreen {
    transitioned: bool,
    finished_at: Option<Instant>,
}

impl LikingScreen {
    pub fn new() -> Self {
        Self { transitioned: false, finished_at: None }
    }
}

impl Screen for LikingScreen {
    fn tick(&mut self, shared: &Arc<SharedState>) -> ScreenResult {
        if self.transitioned {
            return ScreenResult::Stay;
        }
        let is_finished = shared.like.lock().unwrap()
            .as_ref()
            .map(|s| s.lock().unwrap().is_finished)
            .unwrap_or(false);

        if is_finished {
            let finished_at = self.finished_at.get_or_insert_with(Instant::now);
            if finished_at.elapsed() >= std::time::Duration::from_secs(3) {
                self.transitioned = true;
                return ScreenResult::Push(Box::new(crate::tui::screens::next_action::NextActionScreen::new()));
            }
        }
        ScreenResult::Stay
    }

    fn handle_key(&mut self, key: KeyEvent, shared: &Arc<SharedState>) -> ScreenResult {
        if key.code == KeyCode::Char('q') || key.code == KeyCode::Esc {
            let is_finished = shared.like.lock().unwrap()
                .as_ref()
                .map(|s| s.lock().unwrap().is_finished)
                .unwrap_or(true);

            if is_finished {
                return ScreenResult::Push(Box::new(crate::tui::screens::next_action::NextActionScreen::new()));
            }

            if let Some(like) = shared.like.lock().unwrap().as_ref() {
                like.lock().unwrap().should_quit = true;
            }
            return ScreenResult::Pop;
        }
        ScreenResult::Stay
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, shared: &Arc<SharedState>) {
        let like_arc = shared.like.lock().unwrap().clone();
        let Some(like_arc) = like_arc else {
            let p = Paragraph::new("No like/unlike run in progress.");
            frame.render_widget(p, area);
            return;
        };
        let s = like_arc.lock().unwrap();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(6),
                Constraint::Min(5),
                Constraint::Length(3),
            ])
            .split(area);

        let action = if s.liking { "Liking" } else { "Unliking" };
        let header_text = format!(
            " {} content\n\
             Target User: @{} | Status: {}\n\
             Processed : Posts: {} | Chats: {} | Stories: {}\n\
             Changed: {} | Already correct (skipped): {} | Failed: {}",
            action, s.username, s.status, s.posts_processed, s.chats_processed, s.stories_processed,
            s.changed, s.skipped, s.failed
        );
        let header = Paragraph::new(header_text)
            .block(Block::default().borders(Borders::ALL).title(" Statistics ").border_style(Style::default().fg(Color::Cyan)));
        frame.render_widget(header, chunks[0]);

        let log_items: Vec<ListItem> = s.logs.iter().rev().map(|l| ListItem::new(l.as_str())).collect();
        let log_list = List::new(log_items)
            .block(Block::default().borders(Borders::ALL).title(" Log ").border_style(Style::default().fg(Color::Magenta)));
        frame.render_widget(log_list, chunks[1]);

        let quit_text = if s.is_finished {
            "Done. Press [Q] or [Esc] to continue."
        } else {
            "In progress... Press [Q] or [Esc] to abort."
        };
        let footer = Paragraph::new(format!(" {}", quit_text))
            .block(Block::default().borders(Borders::ALL).title(" Controls ").border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(footer, chunks[2]);
    }
}
