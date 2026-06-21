use std::sync::Arc;

use crossterm::event::KeyEvent;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::tui::{
    app::SharedState,
    screens::{Screen, ScreenResult},
    types::{Choice, ListItem, WidgetAction, WidgetResult},
    widgets::list_select::ListSelect,
};

const GITHUB_REPO: &str = "https://github.com/agnosto/of-scraper-rs";

pub struct MainMenuScreen {
    list: ListSelect,
}

impl MainMenuScreen {
    pub fn new() -> Self {
        let items = vec![
            ListItem::Choice(Choice::new("Scrape content")),
            ListItem::Choice(Choice::new("Like/Unlike content")),
            ListItem::Separator(None),
            ListItem::Choice(Choice::new("Donate")),
            ListItem::Choice(Choice::new("Quit")),
        ];
        Self { list: ListSelect::new("Main Menu: what would you like to do?", items) }
    }
}

impl Screen for MainMenuScreen {
    fn handle_key(&mut self, key: KeyEvent, shared: &Arc<SharedState>) -> ScreenResult {
        match self.list.handle_key(key) {
            WidgetAction::Submit => {
                if let WidgetResult::SingleSelect(value) = self.list.result() {
                    match value.as_str() {
                        "Quit" => ScreenResult::Quit,
                        "Donate" => ScreenResult::Push(Box::new(
                            crate::tui::screens::donate::DonateScreen::new(),
                        )),
                        "Scrape content" => {
                            // Reset any stale wizard state and kick off the
                            // subscriptions fetch before the picker even
                            // renders, so it isn't sitting idle.
                            *shared.wizard.lock().unwrap() = Default::default();
                            shared.wizard.lock().unwrap().mode = crate::tui::app::WizardMode::Scrape;
                            ScreenResult::Push(Box::new(
                                crate::tui::screens::user_select::UserSelectScreen::new(),
                            ))
                        }
                        "Like/Unlike content" => {
                            *shared.wizard.lock().unwrap() = Default::default();
                            shared.wizard.lock().unwrap().mode = crate::tui::app::WizardMode::Like;
                            ScreenResult::Push(Box::new(
                                crate::tui::screens::user_select::UserSelectScreen::new(),
                            ))
                        }
                        _ => ScreenResult::Stay,
                    }
                } else {
                    ScreenResult::Stay
                }
            }
            WidgetAction::Cancel => ScreenResult::Quit,
            _ => ScreenResult::Stay,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, shared: &Arc<SharedState>) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(7), Constraint::Min(7), Constraint::Length(3)])
            .split(area);

        let banner = Paragraph::new(vec![

            Line::from(Span::styled(

                r"   ____  ______   _____                                                ",

                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),

            )),

            Line::from(Span::styled(

                r"  / __ \/ ____/  / ___/______________ _____  ___  _____      __________",

                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),

            )),

            Line::from(Span::styled(

                r" / / / / /_______\__ \/ ___/ ___/ __ `/ __ \/ _ \/ ___/_____/ ___/ ___/",

                Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),

            )),

            Line::from(Span::styled(

                r"/ /_/ / __/_____/__/ / /__/ /  / /_/ / /_/ /  __/ /  /_____/ /  (__  ) ",

                Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),

            )),

            Line::from(Span::styled(

                r"\____/_/       /____/\___/_/   \__,_/ .___/\___/_/        /_/  /____/  ",

                Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),

            )),

            Line::from(Span::styled(

                r"                                   /_/",

                Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),

            )),

        ]);
        frame.render_widget(banner, chunks[0]);

        self.list.render(frame, chunks[1]);

        let footer_text = format!(
            "Repo: {}   |   Config: {}\nLog: {}",
            GITHUB_REPO,
            shared.config_path.display(),
            shared.log_path.display()
        );
        let footer = Paragraph::new(footer_text).style(Style::default().fg(Color::DarkGray));
        frame.render_widget(footer, chunks[2]);
    }
}
