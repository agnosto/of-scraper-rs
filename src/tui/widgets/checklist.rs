use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::tui::types::{WidgetAction, WidgetResult};

pub struct CheckItem {
    pub label: String,
    pub value: String,
    pub checked: bool,
}

impl CheckItem {
    pub fn new(label: impl Into<String>, value: impl Into<String>, checked: bool) -> Self {
        Self { label: label.into(), value: value.into(), checked }
    }
}

/// Multi-select checklist.
///
/// Keybinds (same navigation as `ListSelect`, since there's no text input
/// here to collide with j/k):
///   Up/Down or k/j -> move cursor
///   Space           -> toggle current item
///   Enter           -> confirm selection
///   Esc             -> cancel / go back
pub struct Checklist {
    pub message: String,
    items: Vec<CheckItem>,
    state: ListState,
    cursor: usize,
}

impl Checklist {
    pub fn new(message: impl Into<String>, items: Vec<CheckItem>) -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self { message: message.into(), items, state, cursor: 0 }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> WidgetAction {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_cursor(-1);
                WidgetAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_cursor(1);
                WidgetAction::None
            }
            KeyCode::Char(' ') => {
                if let Some(item) = self.items.get_mut(self.cursor) {
                    item.checked = !item.checked;
                }
                WidgetAction::Toggle
            }
            KeyCode::Enter => WidgetAction::Submit,
            KeyCode::Esc => WidgetAction::Cancel,
            _ => WidgetAction::None,
        }
    }

    pub fn result(&self) -> WidgetResult {
        let selected: Vec<String> = self.items.iter().filter(|i| i.checked).map(|i| i.value.clone()).collect();
        WidgetResult::MultiSelect(selected)
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(3),
                Constraint::Length(1),
            ])
            .split(area);

        let msg = Paragraph::new(self.message.as_str()).style(Style::default().fg(Color::Cyan));
        frame.render_widget(msg, chunks[0]);

        let list_items: Vec<ListItem> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let cb = if item.checked { "[x]" } else { "[ ]" };
                let style = if i == self.cursor {
                    Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                ListItem::new(Line::from(vec![
                    Span::styled(if i == self.cursor { "❯ " } else { "  " }, Style::default().fg(Color::Cyan)),
                    Span::styled(format!("{} {}", cb, item.label), style),
                ]))
            })
            .collect();

        let list = List::new(list_items)
            .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));
        frame.render_stateful_widget(list, chunks[1], &mut self.state);

        let hint = Paragraph::new("  [↑↓ / jk] navigate   [Space] toggle   [Enter] confirm   [Esc] back")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(hint, chunks[2]);
    }

    fn move_cursor(&mut self, delta: i32) {
        let len = self.items.len();
        if len == 0 {
            return;
        }
        let next = (self.cursor as i32 + delta).rem_euclid(len as i32) as usize;
        self.cursor = next;
        self.state.select(Some(next));
    }
}
