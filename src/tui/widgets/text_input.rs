use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::tui::types::{WidgetAction, WidgetResult};

pub struct TextInput {
    pub message: String,
    pub input: String,
    pub cursor: usize,
}

impl TextInput {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            input: String::new(),
            cursor: 0,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> WidgetAction {
        match key.code {
            KeyCode::Char(c) => {
                self.input.insert(self.cursor, c);
                self.cursor += c.len_utf8();
                WidgetAction::None
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let prev_char_idx = self.input[..self.cursor]
                        .chars()
                        .last()
                        .map(|c| self.cursor - c.len_utf8())
                        .unwrap_or(0);
                    self.input.remove(prev_char_idx);
                    self.cursor = prev_char_idx;
                }
                WidgetAction::None
            }
            KeyCode::Delete => {
                if self.cursor < self.input.len() {
                    self.input.remove(self.cursor);
                }
                WidgetAction::None
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    let prev_char_idx = self.input[..self.cursor]
                        .chars()
                        .last()
                        .map(|c| self.cursor - c.len_utf8())
                        .unwrap_or(0);
                    self.cursor = prev_char_idx;
                }
                WidgetAction::None
            }
            KeyCode::Right => {
                if self.cursor < self.input.len() {
                    let next_char_len = self.input[self.cursor..]
                        .chars()
                        .next()
                        .map(|c| c.len_utf8())
                        .unwrap_or(0);
                    self.cursor += next_char_len;
                }
                WidgetAction::None
            }
            KeyCode::Enter => WidgetAction::Submit,
            KeyCode::Esc => WidgetAction::Cancel,
            _ => WidgetAction::None,
        }
    }

    pub fn result(&self) -> WidgetResult {
        WidgetResult::Text(self.input.clone())
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(area);

        let msg = Paragraph::new(self.message.as_str()).style(Style::default().fg(Color::Cyan));
        frame.render_widget(msg, chunks[0]);

        let before = &self.input[..self.cursor];
        let cursor_char = if self.cursor < self.input.len() {
            let end = self.cursor + self.input[self.cursor..].chars().next().unwrap().len_utf8();
            &self.input[self.cursor..end]
        } else {
            " "
        };
        let after = if self.cursor < self.input.len() {
            let end = self.cursor + self.input[self.cursor..].chars().next().unwrap().len_utf8();
            &self.input[end..]
        } else {
            ""
        };

        let input_line = Line::from(vec![
            Span::raw(before),
            Span::styled(cursor_char, Style::default().bg(Color::Cyan).fg(Color::Black)),
            Span::raw(after),
        ]);

        let input_block = Paragraph::new(input_line)
            .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(input_block, chunks[1]);

        let hint = Paragraph::new("  [Enter] submit   [Esc] back")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(hint, chunks[2]);
    }
}
