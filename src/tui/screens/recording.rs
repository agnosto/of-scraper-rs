use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame,
};

use crate::tui::{app::SharedState, screens::{Screen, ScreenResult}};

fn format_size(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.2} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.2} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.2} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

/// Progress screen for an in-progress livestream recording. [S]/[Q]/[Esc]
/// during an active run asks for confirmation first, then trips the
/// recording's cancellation token; the file is finalized and converted
/// before the engine reports finished.
pub struct RecordingScreen {
    /// Guards against popping more than once once the engine finished.
    transitioned: bool,
    confirming_stop: bool,
    stopped: bool,
}

impl RecordingScreen {
    pub fn new() -> Self {
        Self { transitioned: false, confirming_stop: false, stopped: false }
    }
}

/// Hands the finished recording's one-line result to the creator picker and
/// releases the shared recording slot. Without this, a recording that ends
/// immediately (for example, "not live") would silently bounce back to the
/// picker with no indication of what happened.
fn close_and_pop(shared: &Arc<SharedState>) -> ScreenResult {
    if let Some(recording) = shared.recording.lock().unwrap().take() {
        let outcome = recording.lock().unwrap().outcome.clone();
        if let Some(outcome) = outcome {
            *shared.last_recording.lock().unwrap() = Some(outcome);
        }
    }
    ScreenResult::Pop
}

impl Screen for RecordingScreen {
    fn tick(&mut self, shared: &Arc<SharedState>) -> ScreenResult {
        if self.transitioned {
            return ScreenResult::Stay;
        }
        let is_finished = shared.recording.lock().unwrap()
            .as_ref()
            .map(|s| s.lock().unwrap().is_finished)
            .unwrap_or(false);
        if is_finished {
            self.transitioned = true;
            // Recording ended (finished or stopped): go back to the creator
            // picker with a one-line result of what happened.
            return close_and_pop(shared);
        }
        ScreenResult::Stay
    }

    fn handle_key(&mut self, key: KeyEvent, shared: &Arc<SharedState>) -> ScreenResult {
        if self.confirming_stop {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if let Some(recording) = shared.recording.lock().unwrap().as_ref() {
                        recording.lock().unwrap().cancel.cancel();
                    }
                    self.stopped = true;
                    self.confirming_stop = false;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.confirming_stop = false;
                }
                _ => {}
            }
            return ScreenResult::Stay;
        }

        match key.code {
            KeyCode::Char('s') | KeyCode::Char('S') | KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
                let is_finished = shared.recording.lock().unwrap()
                    .as_ref()
                    .map(|s| s.lock().unwrap().is_finished)
                    .unwrap_or(true);
                if is_finished || self.stopped {
                    return close_and_pop(shared);
                }
                self.confirming_stop = true;
                ScreenResult::Stay
            }
            _ => ScreenResult::Stay,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, shared: &Arc<SharedState>) {
        let recording_arc = shared.recording.lock().unwrap().clone();
        let Some(recording_arc) = recording_arc else {
            frame.render_widget(Paragraph::new("No recording in progress."), area);
            return;
        };
        let s = recording_arc.lock().unwrap();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),
                Constraint::Min(5),
                Constraint::Length(3),
            ])
            .split(area);

        let header_text = format!(
            " Recording livestream\n\
             Creator: @{} (id {}) | Status: {}\n\
             Duration: {:.1}s | Written: {}{}",
            s.username,
            s.user_id,
            s.status,
            s.duration_ms as f64 / 1000.0,
            format_size(s.bytes_written),
            s.current_file
                .as_ref()
                .map(|f| format!(" | File: {f}"))
                .unwrap_or_default(),
        );
        let header = Paragraph::new(header_text)
            .block(Block::default().borders(Borders::ALL).title(" Livestream recorder ").border_style(Style::default().fg(Color::Cyan)));
        frame.render_widget(header, chunks[0]);

        let log_items: Vec<ListItem> = s.logs.iter().rev().map(|l| ListItem::new(l.as_str())).collect();
        let log_list = List::new(log_items)
            .block(Block::default().borders(Borders::ALL).title(" Recording Logs ").border_style(Style::default().fg(Color::Magenta)));
        frame.render_widget(log_list, chunks[1]);

        let quit_text: String = if s.is_finished {
            "Recording finished. Press [Q] or [Esc] to go back.".to_string()
        } else if self.stopped {
            "Stopping: the current file is finalized (and converted, if configured)...".to_string()
        } else {
            "Recording in progress... Press [S]/[Q]/[Esc] to stop.".to_string()
        };
        let footer = Paragraph::new(quit_text)
            .block(Block::default().borders(Borders::ALL).title(" Controls ").border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(footer, chunks[2]);

        if self.confirming_stop {
            let overlay = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(42),
                    Constraint::Length(5),
                    Constraint::Percentage(42),
                ])
                .split(area)[1];
            let overlay = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(15),
                    Constraint::Percentage(70),
                    Constraint::Percentage(15),
                ])
                .split(overlay)[1];
            frame.render_widget(Clear, overlay);
            let confirm = Paragraph::new(
                "Stop this recording? The current file is finalized and converted\n\
                 (if OutputFormat is set). [Y] stop   [N] keep recording",
            )
            .block(Block::default().borders(Borders::ALL).title(" Confirm stop ").border_style(Style::default().fg(Color::Red)));
            frame.render_widget(confirm, overlay);
        }
    }
}
