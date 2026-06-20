use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::tui::types::{WidgetAction, WidgetResult};

/// One entry the fuzzy list can show/select.
#[derive(Clone)]
pub struct FuzzyEntry {
    pub label: String,
    pub value: String,
}

/// Type-to-filter single-select list.
///
/// Because typing IS the input here, navigation can't use j/k (you need
/// those letters in the filter text), so this widget is intentionally the
/// one place in the app where only the arrow keys move the cursor:
///   type any character -> filters the list
///   Backspace           -> remove last filter character
///   Up/Down             -> move cursor within filtered results
///   Enter                -> confirm
///   Esc                  -> cancel / go back
pub struct FuzzySelect {
    pub message: String,
    all_entries: Vec<FuzzyEntry>,
    filter: String,
    filtered: Vec<usize>, // indices into all_entries
    state: ListState,
    cursor: usize,
}

impl FuzzySelect {
    pub fn new(message: impl Into<String>, entries: Vec<FuzzyEntry>) -> Self {
        let filtered: Vec<usize> = (0..entries.len()).collect();
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            message: message.into(),
            all_entries: entries,
            filter: String::new(),
            filtered,
            state,
            cursor: 0,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> WidgetAction {
        match key.code {
            KeyCode::Up => {
                self.move_cursor(-1);
                WidgetAction::None
            }
            KeyCode::Down => {
                self.move_cursor(1);
                WidgetAction::None
            }
            KeyCode::Enter => {
                if self.filtered.is_empty() {
                    WidgetAction::None
                } else {
                    WidgetAction::Submit
                }
            }
            KeyCode::Esc => WidgetAction::Cancel,
            KeyCode::Backspace => {
                self.filter.pop();
                self.refilter();
                WidgetAction::None
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.refilter();
                WidgetAction::None
            }
            _ => WidgetAction::None,
        }
    }

    pub fn result(&self) -> WidgetResult {
        match self.filtered.get(self.cursor) {
            Some(&idx) => WidgetResult::SingleSelect(self.all_entries[idx].value.clone()),
            None => WidgetResult::Cancelled,
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(3),
                Constraint::Min(3),
                Constraint::Length(1),
            ])
            .split(area);

        let msg = Paragraph::new(self.message.as_str()).style(Style::default().fg(Color::Cyan));
        frame.render_widget(msg, chunks[0]);

        let filter_text = format!(" /{}", self.filter);
        let filter_box = Paragraph::new(filter_text)
            .block(Block::default().borders(Borders::ALL).title(" Filter ").border_style(Style::default().fg(Color::Yellow)));
        frame.render_widget(filter_box, chunks[1]);

        let list_items: Vec<ListItem> = self
            .filtered
            .iter()
            .enumerate()
            .map(|(i, &idx)| {
                let entry = &self.all_entries[idx];
                let style = if i == self.cursor {
                    Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                ListItem::new(Line::from(vec![
                    Span::styled(if i == self.cursor { "❯ " } else { "  " }, Style::default().fg(Color::Cyan)),
                    Span::styled(entry.label.clone(), style),
                ]))
            })
            .collect();

        let title = format!(" Creators ({} match{}) ", self.filtered.len(), if self.filtered.len() == 1 { "" } else { "es" });
        let list = List::new(list_items)
            .block(Block::default().borders(Borders::ALL).title(title).border_style(Style::default().fg(Color::DarkGray)));
        frame.render_stateful_widget(list, chunks[2], &mut self.state);

        let hint = Paragraph::new("  type to filter   [↑↓] navigate   [Enter] select   [Esc] back")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(hint, chunks[3]);
    }

    fn refilter(&mut self) {
        let needle = self.filter.to_lowercase();
        self.filtered = self
            .all_entries
            .iter()
            .enumerate()
            .filter(|(_, e)| needle.is_empty() || e.label.to_lowercase().contains(&needle) || e.value.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect();
        self.cursor = 0;
        self.state.select(Some(0));
    }

    fn move_cursor(&mut self, delta: i32) {
        let len = self.filtered.len();
        if len == 0 {
            return;
        }
        let next = (self.cursor as i32 + delta).rem_euclid(len as i32) as usize;
        self.cursor = next;
        self.state.select(Some(next));
    }
}
