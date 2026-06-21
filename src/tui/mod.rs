pub mod app;
pub mod screens;
pub mod types;
pub mod widgets;

use std::{io, path::PathBuf, sync::Arc, time::Duration};

use crossterm::{
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use app::SharedState;
use screens::{main_menu::MainMenuScreen, Screen, ScreenResult};

pub fn run_tui(
    client: of_client::OFClient,
    downloader: Arc<crate::downloader::Downloader>,
    download_path: PathBuf,
    config_path: PathBuf,
    log_path: PathBuf,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let shared = Arc::new(SharedState::new(client, downloader, download_path, config_path, log_path));
    let mut stack: Vec<Box<dyn Screen>> = vec![Box::new(MainMenuScreen::new())];

    loop {
        // Let the top screen react to any background state change (loads
        // finishing, scrape progress, etc) even with no key event.
        if let Some(top) = stack.last_mut() {
            match top.tick(&shared) {
                ScreenResult::Push(screen) => stack.push(screen),
                ScreenResult::Pop => {
                    stack.pop();
                }
                ScreenResult::Reset(screen) => {
                    stack.clear();
                    stack.push(screen);
                }
                ScreenResult::Quit => break,
                ScreenResult::Stay => {}
            }
        }
        if stack.is_empty() {
            break;
        }

        terminal.draw(|f| {
            let area = f.area();
            if let Some(top) = stack.last_mut() {
                top.render(f, area, &shared);
            }
        })?;

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat {
                if let Some(top) = stack.last_mut() {
                    match top.handle_key(key, &shared) {
                        ScreenResult::Push(screen) => stack.push(screen),
                        ScreenResult::Pop => {
                            stack.pop();
                            if stack.is_empty() {
                                break;
                            }
                        }
                        ScreenResult::Reset(screen) => {
                            stack.clear();
                            stack.push(screen);
                        }
                        ScreenResult::Quit => break,
                        ScreenResult::Stay => {}
                    }
                }
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
