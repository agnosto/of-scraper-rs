use std::{
    collections::HashMap,
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Terminal,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Menu,
    Scraping,
}

pub struct ActiveDownload {
    pub filename: String,
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
}

pub struct AppState {
    // Menu state
    pub screen: Screen,
    pub menu_username: String,
    pub menu_posts: bool,
    pub menu_chats: bool,
    pub menu_stories: bool,
    pub menu_focus: usize, // 0 = Username, 1 = Posts, 2 = Chats, 3 = Stories, 4 = Start Button
    pub start_engine: bool,

    // Scraping state
    pub username: String,
    pub status: String,
    pub posts_scraped: usize,
    pub chats_scraped: usize,
    pub stories_scraped: usize,
    pub files_downloaded: usize,
    pub files_failed: usize,
    pub active_downloads: HashMap<u64, ActiveDownload>,
    pub logs: Vec<String>,
    pub download_path: PathBuf,
    pub is_finished: bool,
    pub should_quit: bool,
}

impl AppState {
    pub fn new(username: String, download_path: PathBuf) -> Self {
        Self {
            screen: Screen::Menu,
            menu_username: username.clone(),
            menu_posts: true,
            menu_chats: true,
            menu_stories: true,
            menu_focus: 0,
            start_engine: false,

            username,
            status: "Waiting to start...".to_string(),
            posts_scraped: 0,
            chats_scraped: 0,
            stories_scraped: 0,
            files_downloaded: 0,
            files_failed: 0,
            active_downloads: HashMap::new(),
            logs: Vec::new(),
            download_path,
            is_finished: false,
            should_quit: false,
        }
    }

    pub fn log(&mut self, msg: String) {
        if self.logs.len() > 100 {
            self.logs.remove(0);
        }
        self.logs.push(msg);
    }
}

pub fn log_message(state: &Option<Arc<Mutex<AppState>>>, msg: &str) {
    if let Some(state) = state {
        state.lock().unwrap().log(msg.to_string());
    } else {
        println!("{}", msg);
    }
}

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

pub fn run_tui(
    state: Arc<Mutex<AppState>>,
    client: of_client::OFClient,
    downloader: Arc<crate::downloader::Downloader>,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    terminal.clear()?;

    loop {
        // Trigger the scrape engine if menu clicked start
        let run_engine = {
            let mut s = state.lock().unwrap();
            if s.start_engine {
                s.start_engine = false;
                s.screen = Screen::Scraping;
                true
            } else {
                false
            }
        };

        if run_engine {
            let client_clone = client.clone();
            let downloader_clone = downloader.clone();
            let state_clone = state.clone();
            tokio::spawn(async move {
                let (user, content_types) = {
                    let s = state_clone.lock().unwrap();
                    let mut cts = Vec::new();
                    if s.menu_posts { cts.push("posts".to_string()); }
                    if s.menu_chats { cts.push("chats".to_string()); }
                    if s.menu_stories { cts.push("stories".to_string()); }
                    (s.menu_username.clone(), cts)
                };
                let _ = crate::run_scrape_engine_tui(client_clone, downloader_clone, user, content_types, state_clone).await;
            });
        }

        // Draw the interface
        terminal.draw(|f| {
            let size = f.area();
            let s = state.lock().unwrap();

            match s.screen {
                Screen::Menu => {
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(3),
                            Constraint::Min(10),
                            Constraint::Length(3),
                        ])
                        .split(size);

                    let banner = Paragraph::new(" OnlyFans Scraper TUI Setup ")
                        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Cyan)));
                    f.render_widget(banner, chunks[0]);

                    let focus = s.menu_focus;
                    let username_style = if focus == 0 { Style::default().fg(Color::Yellow).add_modifier(Modifier::REVERSED) } else { Style::default() };
                    let posts_style = if focus == 1 { Style::default().fg(Color::Yellow).add_modifier(Modifier::REVERSED) } else { Style::default() };
                    let chats_style = if focus == 2 { Style::default().fg(Color::Yellow).add_modifier(Modifier::REVERSED) } else { Style::default() };
                    let stories_style = if focus == 3 { Style::default().fg(Color::Yellow).add_modifier(Modifier::REVERSED) } else { Style::default() };
                    let start_style = if focus == 4 { Style::default().fg(Color::Green).add_modifier(Modifier::REVERSED) } else { Style::default().fg(Color::Green) };

                    let posts_cb = if s.menu_posts { "[X]" } else { "[ ]" };
                    let chats_cb = if s.menu_chats { "[X]" } else { "[ ]" };
                    let stories_cb = if s.menu_stories { "[X]" } else { "[ ]" };

                    use ratatui::text::{Line, Span};
                    let lines = vec![
                        Line::from(""),
                        Line::from(Span::raw(" 1. Username to Scrape:")),
                        Line::from(Span::styled(format!("    [ {} ]", s.menu_username), username_style)),
                        Line::from(""),
                        Line::from(Span::raw(" 2. Content Types to Download (Space or Enter to toggle):")),
                        Line::from(Span::styled(format!("    {} Posts", posts_cb), posts_style)),
                        Line::from(Span::styled(format!("    {} Chats", chats_cb), chats_style)),
                        Line::from(Span::styled(format!("    {} Stories", stories_cb), stories_style)),
                        Line::from(""),
                        Line::from(""),
                        Line::from(Span::styled("   [ START SCRAPING ]", start_style)),
                    ];

                    let menu_block = Paragraph::new(lines)
                        .block(Block::default().borders(Borders::ALL).title(" Scrape Configuration ").border_style(Style::default().fg(Color::Blue)));
                    f.render_widget(menu_block, chunks[1]);

                    let footer = Paragraph::new(" [Tab/Up/Down] Navigate | [Space/Enter] Toggle/Select | [Q/Esc] Quit")
                        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));
                    f.render_widget(footer, chunks[2]);
                }
                Screen::Scraping => {
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(6),
                            Constraint::Min(5),
                            Constraint::Length(3),
                        ])
                        .split(size);

                    let header_text = format!(
                        " OnlyFans Scraper CLI/TUI\n\
                         Target User: @{} | Status: {}\n\
                         Scraped   : Posts: {} | Chats: {} | Stories: {}\n\
                         Downloads : Succeeded: {} | Failed: {}",
                        s.username,
                        s.status,
                        s.posts_scraped,
                        s.chats_scraped,
                        s.stories_scraped,
                        s.files_downloaded,
                        s.files_failed
                    );

                    let header = Paragraph::new(header_text)
                        .block(Block::default().borders(Borders::ALL).title(" Statistics ").border_style(Style::default().fg(Color::Cyan)));
                    f.render_widget(header, chunks[0]);

                    let body_chunks = Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([
                            Constraint::Percentage(50),
                            Constraint::Percentage(50),
                        ])
                        .split(chunks[1]);

                    let mut dl_items = Vec::new();
                    for dl in s.active_downloads.values() {
                        let progress_text = if let Some(total) = dl.total_bytes {
                            let percent = if total > 0 { (dl.bytes_downloaded as f64 / total as f64) * 100.0 } else { 0.0 };
                            format!("{} / {} ({:.1}%)", format_size(dl.bytes_downloaded), format_size(total), percent)
                        } else {
                            format!("{} / Unknown (DRM/FFmpeg downloading...)", format_size(dl.bytes_downloaded))
                        };
                        dl_items.push(ListItem::new(format!("• {} \n  {}", dl.filename, progress_text)));
                    }

                    if dl_items.is_empty() {
                        dl_items.push(ListItem::new("No active downloads"));
                    }

                    let dl_list = List::new(dl_items)
                        .block(Block::default().borders(Borders::ALL).title(" Active Downloads ").border_style(Style::default().fg(Color::Blue)));
                    f.render_widget(dl_list, body_chunks[0]);

                    let log_items: Vec<ListItem> = s.logs.iter().rev().map(|log| ListItem::new(log.as_str())).collect();
                    let log_list = List::new(log_items)
                        .block(Block::default().borders(Borders::ALL).title(" Scraper Logs ").border_style(Style::default().fg(Color::Magenta)));
                    f.render_widget(log_list, body_chunks[1]);

                    let quit_text = if s.is_finished {
                        "Scrape completed. Press [Q] or [Esc] to exit."
                    } else {
                        "Scraping in progress... Press [Q] or [Esc] to abort."
                    };
                    let footer_text = format!(
                        " {} | Destination: {}",
                        quit_text,
                        s.download_path.to_string_lossy()
                    );
                    let footer = Paragraph::new(footer_text)
                        .block(Block::default().borders(Borders::ALL).title(" Controls ").border_style(Style::default().fg(Color::DarkGray)));
                    f.render_widget(footer, chunks[2]);
                }
            }
        })?;

        // Handle Keyboard Events
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                let mut s = state.lock().unwrap();

                match s.screen {
                    Screen::Menu => match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            s.should_quit = true;
                            break;
                        }
                        KeyCode::Tab | KeyCode::Down => {
                            s.menu_focus = (s.menu_focus + 1) % 5;
                        }
                        KeyCode::Up => {
                            if s.menu_focus > 0 {
                                s.menu_focus -= 1;
                            } else {
                                s.menu_focus = 4;
                            }
                        }
                        KeyCode::Char(' ') => match s.menu_focus {
                            1 => s.menu_posts = !s.menu_posts,
                            2 => s.menu_chats = !s.menu_chats,
                            3 => s.menu_stories = !s.menu_stories,
                            4 => {
                                s.start_engine = true;
                            }
                            _ => {}
                        },
                        KeyCode::Enter => match s.menu_focus {
                            4 => {
                                s.start_engine = true;
                            }
                            0 => {
                                s.menu_focus = 1;
                            }
                            1 => s.menu_posts = !s.menu_posts,
                            2 => s.menu_chats = !s.menu_chats,
                            3 => s.menu_stories = !s.menu_stories,
                            _ => {}
                        },
                        KeyCode::Char(c) if s.menu_focus == 0 => {
                            s.menu_username.push(c);
                        }
                        KeyCode::Backspace if s.menu_focus == 0 => {
                            s.menu_username.pop();
                        }
                        _ => {}
                    },
                    Screen::Scraping => {
                        if key.code == KeyCode::Char('q') || key.code == KeyCode::Esc {
                            s.should_quit = true;
                            break;
                        }
                    }
                }
            }
        }

        let mut s = state.lock().unwrap();
        if s.screen == Screen::Scraping && s.is_finished && s.active_downloads.is_empty() {
            s.status = "Finished".to_string();
        }
        if s.should_quit {
            break;
        }
    }

    // Cleanup Raw Mode and screen
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
