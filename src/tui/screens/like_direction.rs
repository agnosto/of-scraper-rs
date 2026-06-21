use std::sync::{Arc, Mutex};

use crossterm::event::KeyEvent;
use ratatui::{layout::Rect, Frame};

use crate::tui::{
    app::SharedState,
    screens::{Screen, ScreenResult},
    types::{Choice, ListItem, WidgetAction, WidgetResult},
    widgets::list_select::ListSelect,
};

pub struct LikeDirectionScreen {
    list: ListSelect,
}

impl LikeDirectionScreen {
    pub fn new() -> Self {
        let items = vec![
            ListItem::Choice(Choice::new("Like all")),
            ListItem::Choice(Choice::new("Unlike all")),
        ];
        Self { list: ListSelect::new("Like or unlike everything selected?", items) }
    }
}

impl Screen for LikeDirectionScreen {
    fn handle_key(&mut self, key: KeyEvent, shared: &Arc<SharedState>) -> ScreenResult {
        match self.list.handle_key(key) {
            WidgetAction::Submit => {
                if let WidgetResult::SingleSelect(value) = self.list.result() {
                    let liking = value == "Like all";

                    let (username, content_types) = {
                        let wizard = shared.wizard.lock().unwrap();
                        (wizard.username.clone(), wizard.content_types.clone())
                    };
                    let Some(username) = username else { return ScreenResult::Pop };

                    let like_state = Arc::new(Mutex::new(crate::tui::app::LikeState::new(username.clone(), liking)));
                    *shared.like.lock().unwrap() = Some(like_state.clone());

                    let client = shared.client.clone();
                    tokio::spawn(async move {
                        let _ = crate::run_like_engine_tui(client, username, content_types, liking, like_state).await;
                    });

                    return ScreenResult::Push(Box::new(crate::tui::screens::liking::LikingScreen::new()));
                }
                ScreenResult::Stay
            }
            WidgetAction::Cancel => ScreenResult::Pop,
            _ => ScreenResult::Stay,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _shared: &Arc<SharedState>) {
        self.list.render(frame, area);
    }
}
