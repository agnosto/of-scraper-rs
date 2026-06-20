use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::tui::types::{Choice, ListItem as Item, WidgetAction, WidgetResult};

/// Single-select list prompt.
///
/// Keybinds (kept identical across every list/select widget in this app):
///   Up/Down or k/j  -> move cursor
///   Enter           -> confirm
///   Esc             -> cancel / go back
pub struct ListSelect {
    pub message: String,
    pub items: Vec<Item>,
    state: ListState,
    /// Cursor tracked separately because separators aren't selectable.
    cursor: usize,
}

impl ListSelect {
    pub fn new(message: impl Into<String>, items: Vec<Item>) -> Self {
        let mut state = ListState::default();
        let first_selectable = items
            .iter()
            .position(|i| matches!(i, Item::Choice(_)))
            .unwrap_or(0);
        state.select(Some(first_selectable));
        Self { message: message.into(), items, state, cursor: first_selectable }
    }

    pub fn with_default(mut self, value: &str) -> Self {
        for (i, item) in self.items.iter().enumerate() {
            if let Item::Choice(Choice { value: v, .. }) = item {
                if v == value {
                    self.cursor = i;
                    self.state.select(Some(i));
                    break;
                }
            }
        }
        self
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
            KeyCode::Enter => WidgetAction::Submit,
            KeyCode::Esc => WidgetAction::Cancel,
            _ => WidgetAction::None,
        }
    }

    pub fn result(&self) -> WidgetResult {
        if let Some(Item::Choice(c)) = self.items.get(self.cursor) {
            WidgetResult::SingleSelect(c.value.clone())
        } else {
            WidgetResult::Cancelled
        }
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
            .map(|(i, item)| match item {
                Item::Choice(c) => {
                    let style = if i == self.cursor {
                        Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(if i == self.cursor { "❯ " } else { "  " }, Style::default().fg(Color::Cyan)),
                        Span::styled(c.label.clone(), style),
                    ]))
                }
                Item::Separator(label) => {
                    let text = label.as_deref().map(|s| format!("── {} ", s)).unwrap_or_else(|| "──────────────".into());
                    ListItem::new(Line::from(Span::styled(text, Style::default().fg(Color::DarkGray))))
                }
            })
            .collect();

        let list = List::new(list_items)
            .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));

        frame.render_stateful_widget(list, chunks[1], &mut self.state);

        let hint = Paragraph::new("  [↑↓ / jk] navigate   [Enter] select   [Esc] back")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(hint, chunks[2]);
    }

    fn move_cursor(&mut self, delta: i32) {
        let len = self.items.len();
        if len == 0 {
            return;
        }
        let mut next = (self.cursor as i32 + delta).rem_euclid(len as i32) as usize;
        let mut attempts = 0;
        while matches!(self.items.get(next), Some(Item::Separator(_))) && attempts < len {
            next = (next as i32 + delta.signum()).rem_euclid(len as i32) as usize;
            attempts += 1;
        }
        self.cursor = next;
        self.state.select(Some(next));
    }
}
