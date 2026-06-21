pub mod main_menu;
pub mod user_select;
pub mod content_select;
pub mod scraping;
pub mod donate;
pub mod next_action;
pub mod like_options;
pub mod like_direction;
pub mod liking;

use std::sync::Arc;

use crossterm::event::KeyEvent;
use ratatui::{layout::Rect, Frame};

use crate::tui::app::SharedState;

/// What a screen wants the navigation stack to do after handling a key.
pub enum ScreenResult {
    /// Nothing to do, keep showing this screen.
    Stay,
    /// Push a new screen on top (e.g. menu -> creator picker).
    Push(Box<dyn Screen>),
    /// Pop back to whatever was underneath this screen.
    Pop,
    /// Clear the entire stack and start fresh from the given screen — used
    /// by the post-scrape "what next" screen so "scrape another creator"
    /// or "back to menu" don't leave a pile of stale wizard screens
    /// underneath.
    Reset(Box<dyn Screen>),
    /// Tear down the whole TUI.
    Quit,
}

pub trait Screen {
    fn handle_key(&mut self, key: KeyEvent, shared: &Arc<SharedState>) -> ScreenResult;
    fn render(&mut self, frame: &mut Frame, area: Rect, shared: &Arc<SharedState>);

    /// Called once per loop iteration regardless of key events, so screens
    /// can react to background state changing (subscriptions finishing
    /// loading, scrape progress ticking, etc). Default no-op.
    fn tick(&mut self, _shared: &Arc<SharedState>) -> ScreenResult {
        ScreenResult::Stay
    }
}
