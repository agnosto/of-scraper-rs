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

pub struct MainMenuScreen {
    list: ListSelect,
}

impl MainMenuScreen {
    pub fn new() -> Self {
        let items = vec![
            ListItem::Choice(Choice::new("Scrape a specific creator")),
            ListItem::Choice(Choice::new("Scrape ALL purchased content")),
            ListItem::Choice(Choice::new("Download from link(s)")),
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
        // [U] opens the release page for a known update notice (pre-check
        // so an unknown key still falls through to the menu list).
        if matches!(key.code, crossterm::event::KeyCode::Char('u') | crossterm::event::KeyCode::Char('U'))
            && let Some(info) = shared.update.lock().unwrap().clone()
        {
            crate::updater::open_url(&info.url);
            return ScreenResult::Stay;
        }
        match self.list.handle_key(key) {
            WidgetAction::Submit => {
                if let WidgetResult::SingleSelect(value) = self.list.result() {
                    match value.as_str() {
                        "Quit" => ScreenResult::Quit,
                        "Donate" => ScreenResult::Push(Box::new(
                            crate::tui::screens::donate::DonateScreen::new(),
                        )),
                        "Scrape a specific creator" => {
                            // Reset any stale wizard state and kick off the
                            // subscriptions fetch before the picker even
                            // renders, so it isn't sitting idle.
                            *shared.wizard.lock().unwrap() = Default::default();
                            shared.wizard.lock().unwrap().mode = crate::tui::app::WizardMode::Scrape;
                            ScreenResult::Push(Box::new(
                                crate::tui::screens::user_select::UserSelectScreen::new(),
                            ))
                        }
                        "Scrape ALL purchased content" => {
                            *shared.wizard.lock().unwrap() = Default::default();
                            let scrape_state = Arc::new(std::sync::Mutex::new(crate::tui::app::ScrapeState::new(
                                "All Purchases".to_string(),
                                shared.download_path.clone(),
                            )));
                            *shared.scrape.lock().unwrap() = Some(scrape_state.clone());

                            let client = shared.client.clone();
                            let downloader = shared.downloader.clone();
                            tokio::spawn(async move {
                                let _ = crate::run_scrape_all_purchases_tui(client, downloader, scrape_state).await;
                            });
                            ScreenResult::Push(Box::new(
                                crate::tui::screens::scraping::ScrapingScreen::new(),
                            ))
                        }
                        "Download from link(s)" => {
                            ScreenResult::Push(Box::new(
                                crate::tui::screens::link_input::LinkInputScreen::new(),
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
        let update = shared.update.lock().unwrap().clone();
        let mut footer_lines: Vec<Line> = Vec::new();
        let footer_height = if update.is_some() { 4 } else { 3 };
        if let Some(info) = update {
            footer_lines.push(Line::from(Span::styled(
                format!(
                    "Update available: {} (installed: v{}) — press [U] to open the release page",
                    info.latest,
                    crate::updater::CURRENT_VERSION
                ),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )));
        }
        footer_lines.push(Line::from(Span::styled(
            format!("Repo: https://github.com/agnosto/of-scraper-rs   |   Config: {}", shared.config_path.display()),
            Style::default().fg(Color::DarkGray),
        )));
        footer_lines.push(Line::from(Span::styled(
            format!("Log: {}", shared.log_path.display()),
            Style::default().fg(Color::DarkGray),
        )));

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(7), Constraint::Min(7), Constraint::Length(footer_height)])
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

        let footer = Paragraph::new(footer_lines).style(Style::default().fg(Color::DarkGray));
        frame.render_widget(footer, chunks[2]);
    }
}
