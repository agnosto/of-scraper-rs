use std::sync::Arc;

use crossterm::event::KeyEvent;
use ratatui::{layout::Rect, Frame};

use crate::tui::{
    app::SharedState,
    screens::{Screen, ScreenResult},
    types::{WidgetAction, WidgetResult},
    widgets::checklist::{CheckItem, Checklist},
};

pub struct LikeOptionsScreen {
    checklist: Checklist,
}

impl LikeOptionsScreen {
    pub fn new() -> Self {
        let items = vec![
            CheckItem::new("Posts", "posts", true),
            CheckItem::new("Chats / Messages", "chats", false),
            CheckItem::new("Stories", "stories", false),
        ];
        Self { checklist: Checklist::new("Choose what content to affect (Space to toggle):", items) }
    }
}

impl Screen for LikeOptionsScreen {
    fn handle_key(&mut self, key: KeyEvent, shared: &Arc<SharedState>) -> ScreenResult {
        match self.checklist.handle_key(key) {
            WidgetAction::Submit => {
                if let WidgetResult::MultiSelect(types) = self.checklist.result() {
                    if types.is_empty() {
                        return ScreenResult::Stay;
                    }
                    shared.wizard.lock().unwrap().content_types = types;
                    return ScreenResult::Push(Box::new(crate::tui::screens::like_direction::LikeDirectionScreen::new()));
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
