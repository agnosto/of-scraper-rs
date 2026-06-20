use std::sync::Arc;

use crossterm::event::KeyEvent;
use ratatui::{layout::Rect, Frame};

use crate::tui::{
    app::SharedState,
    screens::{Screen, ScreenResult},
    types::{Choice, ListItem, WidgetAction, WidgetResult},
    widgets::list_select::ListSelect,
};

pub struct NextActionScreen {
    list: ListSelect,
}

impl NextActionScreen {
    pub fn new() -> Self {
        let items = vec![
            ListItem::Choice(Choice::new("Scrape another creator")),
            ListItem::Choice(Choice::new("Return to main menu")),
            ListItem::Separator(None),
            ListItem::Choice(Choice::new("Quit")),
        ];
        Self { list: ListSelect::new("Scrape finished — what would you like to do next?", items) }
    }
}

impl Screen for NextActionScreen {
    fn handle_key(&mut self, key: KeyEvent, shared: &Arc<SharedState>) -> ScreenResult {
        match self.list.handle_key(key) {
            WidgetAction::Submit => {
                if let WidgetResult::SingleSelect(value) = self.list.result() {
                    match value.as_str() {
                        "Quit" => ScreenResult::Quit,
                        "Scrape another creator" => {
                            *shared.wizard.lock().unwrap() = Default::default();
                            *shared.scrape.lock().unwrap() = None;
                            ScreenResult::Reset(Box::new(crate::tui::screens::user_select::UserSelectScreen::new()))
                        }
                        "Return to main menu" => {
                            *shared.scrape.lock().unwrap() = None;
                            ScreenResult::Reset(Box::new(crate::tui::screens::main_menu::MainMenuScreen::new()))
                        }
                        _ => ScreenResult::Stay,
                    }
                } else {
                    ScreenResult::Stay
                }
            }
            // Esc here just falls back to the main menu rather than
            // exiting the program outright — closing the whole TUI from
            // this screen should be a deliberate "Quit" pick.
            WidgetAction::Cancel => {
                *shared.scrape.lock().unwrap() = None;
                ScreenResult::Reset(Box::new(crate::tui::screens::main_menu::MainMenuScreen::new()))
            }
            _ => ScreenResult::Stay,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _shared: &Arc<SharedState>) {
        self.list.render(frame, area);
    }
}
