use std::sync::{Arc, Mutex};
use crossterm::event::KeyEvent;
use ratatui::{layout::Rect, Frame};
use crate::tui::{
    app::SharedState,
    screens::{Screen, ScreenResult},
    types::{WidgetAction, WidgetResult},
    widgets::text_input::TextInput,
};

pub struct LinkInputScreen {
    input: TextInput,
}

impl LinkInputScreen {
    pub fn new() -> Self {
        Self {
            input: TextInput::new("Enter post or message link(s) separated by spaces or commas:"),
        }
    }
}

impl Screen for LinkInputScreen {
    fn handle_key(&mut self, key: KeyEvent, shared: &Arc<SharedState>) -> ScreenResult {
        match self.input.handle_key(key) {
            WidgetAction::Submit => {
                if let WidgetResult::Text(value) = self.input.result() {
                    let urls: Vec<String> = value
                        .split(|c| c == ' ' || c == ',')
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .collect();

                    if urls.is_empty() {
                        return ScreenResult::Stay;
                    }

                    let scrape_state = Arc::new(Mutex::new(crate::tui::app::ScrapeState::new(
                        "Links Download".to_string(),
                        shared.download_path.clone(),
                    )));
                    *shared.scrape.lock().unwrap() = Some(scrape_state.clone());

                    let client = shared.client.clone();
                    let downloader = shared.downloader.clone();
                    tokio::spawn(async move {
                        let _ = crate::run_link_download_tui(client, downloader, urls, scrape_state).await;
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
        self.input.render(frame, area);
    }
}
