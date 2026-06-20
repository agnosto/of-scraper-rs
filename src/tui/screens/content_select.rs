use std::sync::{Arc, Mutex};

use crossterm::event::KeyEvent;
use ratatui::{layout::Rect, Frame};

use crate::tui::{
    app::SharedState,
    screens::{Screen, ScreenResult},
    types::{WidgetAction, WidgetResult},
    widgets::checklist::{CheckItem, Checklist},
};

pub struct ContentSelectScreen {
    checklist: Checklist,
}

impl ContentSelectScreen {
    pub fn new() -> Self {
        let items = vec![
            CheckItem::new("Posts", "posts", true),
            CheckItem::new("Chats / Messages", "chats", true),
            CheckItem::new("Stories", "stories", true),
        ];
        Self { checklist: Checklist::new("Choose what to scrape (Space to toggle):", items) }
    }
}

impl Screen for ContentSelectScreen {
    fn handle_key(&mut self, key: KeyEvent, shared: &Arc<SharedState>) -> ScreenResult {
        match self.checklist.handle_key(key) {
            WidgetAction::Submit => {
                if let WidgetResult::MultiSelect(types) = self.checklist.result() {
                    if types.is_empty() {
                        return ScreenResult::Stay;
                    }

                    let username = shared.wizard.lock().unwrap().username.clone();
                    let Some(username) = username else { return ScreenResult::Pop };

                    let scrape_state = Arc::new(Mutex::new(crate::tui::app::ScrapeState::new(
                        username.clone(),
                        shared.download_path.clone(),
                    )));
                    *shared.scrape.lock().unwrap() = Some(scrape_state.clone());

                    let client = shared.client.clone();
                    let downloader = shared.downloader.clone();
                    tokio::spawn(async move {
                        let _ = crate::run_scrape_engine_tui(client, downloader, username, types, scrape_state).await;
                    });

                    return ScreenResult::Push(Box::new(crate::tui::screens::scraping::ScrapingScreen::new()));
                }
                ScreenResult::Stay
            }
            WidgetAction::Cancel => ScreenResult::Pop,
            _ => ScreenResult::Stay,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _shared: &Arc<SharedState>) {
        self.checklist.render(frame, area);
    }
}
