use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, List, ListItem, Paragraph},
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

pub struct ScrapingScreen {
    /// Guards against pushing `NextActionScreen` more than once — `tick()`
    /// runs every loop iteration, so without this it would try to push a
    /// new copy on every single frame after finishing.
    transitioned: bool,
}

impl ScrapingScreen {
    pub fn new() -> Self {
        Self { transitioned: false }
    }
}

impl Screen for ScrapingScreen {
    fn tick(&mut self, shared: &Arc<SharedState>) -> ScreenResult {
        if self.transitioned {
            return ScreenResult::Stay;
        }
        let is_finished = shared.scrape.lock().unwrap()
            .as_ref()
            .map(|s| s.lock().unwrap().is_finished)
            .unwrap_or(false);

        if is_finished {
            self.transitioned = true;
            return ScreenResult::Push(Box::new(crate::tui::screens::next_action::NextActionScreen::new()));
        }
        ScreenResult::Stay
    }

    fn handle_key(&mut self, key: KeyEvent, shared: &Arc<SharedState>) -> ScreenResult {
        if key.code == KeyCode::Char('q') || key.code == KeyCode::Esc {
            let is_finished = shared.scrape.lock().unwrap()
                .as_ref()
                .map(|s| s.lock().unwrap().is_finished)
                .unwrap_or(true);

            if is_finished {
                return ScreenResult::Push(Box::new(crate::tui::screens::next_action::NextActionScreen::new()));
            }

            if let Some(scrape) = shared.scrape.lock().unwrap().as_ref() {
                scrape.lock().unwrap().should_quit = true;
            }
            return ScreenResult::Pop;
        }
        ScreenResult::Stay
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, shared: &Arc<SharedState>) {
        let scrape_arc = shared.scrape.lock().unwrap().clone();
        let Some(scrape_arc) = scrape_arc else {
            let p = Paragraph::new("No scrape in progress.");
            frame.render_widget(p, area);
            return;
        };
        let s = scrape_arc.lock().unwrap();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(6),
                Constraint::Min(5),
                Constraint::Length(3),
            ])
            .split(area);

        let header_text = format!(
            " OnlyFans Scraper\n\
             Target User: @{} | Status: {}\n\
             Scraped   : Posts: {} | Chats: {} | Stories: {} | Purchases: {} | Highlights: {}\n\
             Downloads : Succeeded: {} | Failed: {}",
            s.username, s.status, s.posts_scraped, s.chats_scraped, s.stories_scraped, s.purchases_scraped, s.highlights_scraped, s.files_downloaded, s.files_failed
        );
        let header = Paragraph::new(header_text)
            .block(Block::default().borders(Borders::ALL).title(" Statistics ").border_style(Style::default().fg(Color::Cyan)));
        frame.render_widget(header, chunks[0]);

        let body_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[1]);

        let mut dl_items = Vec::new();
        for dl in s.active_downloads.values() {
            let progress_text = if let Some(total) = dl.total_bytes {
                let percent = if total > 0 { (dl.bytes_downloaded as f64 / total as f64) * 100.0 } else { 0.0 };
                format!("{} / {} ({:.1}%)", format_size(dl.bytes_downloaded), format_size(total), percent)
            } else {
                format!("{} / Unknown (DRM/FFmpeg downloading...)", format_size(dl.bytes_downloaded))
            };
            dl_items.push(ListItem::new(format!("• {}\n  {}", dl.filename, progress_text)));
        }
        if dl_items.is_empty() {
            dl_items.push(ListItem::new("No active downloads"));
        }
        let dl_list = List::new(dl_items)
            .block(Block::default().borders(Borders::ALL).title(" Active Downloads ").border_style(Style::default().fg(Color::Blue)));
        frame.render_widget(dl_list, body_chunks[0]);

        let log_items: Vec<ListItem> = s.logs.iter().rev().map(|l| ListItem::new(l.as_str())).collect();
        let log_list = List::new(log_items)
            .block(Block::default().borders(Borders::ALL).title(" Scraper Logs ").border_style(Style::default().fg(Color::Magenta)));
        frame.render_widget(log_list, body_chunks[1]);

        let quit_text = if s.is_finished {
            "Scrape completed. Press [Q] or [Esc] to go back."
        } else {
            "Scraping in progress... Press [Q] or [Esc] to abort."
        };
        let footer_text = format!(" {} | Destination: {}", quit_text, s.download_path.to_string_lossy());
        let footer = Paragraph::new(footer_text)
            .block(Block::default().borders(Borders::ALL).title(" Controls ").border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(footer, chunks[2]);
    }
}
