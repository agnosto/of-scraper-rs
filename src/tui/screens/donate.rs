use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent};
use qrcode::QrCode;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::tui::{app::SharedState, screens::{Screen, ScreenResult}};

const BTC_ADDRESS: &str = "bc1q0e78wrtc9ezp6tqv000wfewgqf2ue4tpzdk7ee";
const SOL_ADDRESS: &str = "Bv3kYZcwSTHXAQtnPddTF27D3F6Gc29v2MfFLqmGF6Gf";
const GITHUB_REPO: &str = "https://github.com/agnosto/of-scraper-rs";
const GITHUB_SPONSORS: &str = "https://github.com/sponsors/agnosto";

pub struct DonateScreen;

impl DonateScreen {
    pub fn new() -> Self {
        Self
    }
}

/// Render an address as a small unicode-block QR code, falling back to
/// plain text if encoding ever fails (addresses are short, so it won't).
fn qr_lines(data: &str) -> Vec<Line<'static>> {
    match QrCode::new(data) {
        Ok(code) => {
            let rendered: String = code
                .render::<char>()
                .quiet_zone(true)
                .module_dimensions(1, 1)
                .light_color(' ')
                .dark_color('█')
                .build();
            rendered
                .lines()
                .map(|l| Line::from(Span::raw(l.to_string())))
                .collect()
        }
        Err(_) => vec![Line::from(Span::raw(data.to_string()))],
    }
}

impl Screen for DonateScreen {
    fn handle_key(&mut self, key: KeyEvent, _shared: &Arc<SharedState>) -> ScreenResult {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => ScreenResult::Pop,
            _ => ScreenResult::Stay,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, _shared: &Arc<SharedState>) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(10),
                Constraint::Length(3),
            ])
            .split(area);

        let title = Paragraph::new("Support the Project")
            .style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
            .alignment(Alignment::Center);
        frame.render_widget(title, chunks[0]);

        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[1]);

        let btc_header = Line::from(Span::styled("Bitcoin (BTC)", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)));
        let mut btc_lines = vec![btc_header, Line::from("")];
        btc_lines.extend(qr_lines(BTC_ADDRESS));
        btc_lines.push(Line::from(""));
        btc_lines.push(Line::from(Span::styled(BTC_ADDRESS, Style::default().fg(Color::Magenta))));
        let btc = Paragraph::new(btc_lines).alignment(Alignment::Center);
        frame.render_widget(btc, cols[0]);

        let sol_header = Line::from(Span::styled("Solana (SOL)", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)));
        let mut sol_lines = vec![sol_header, Line::from("")];
        sol_lines.extend(qr_lines(SOL_ADDRESS));
        sol_lines.push(Line::from(""));
        sol_lines.push(Line::from(Span::styled(SOL_ADDRESS, Style::default().fg(Color::Magenta))));
        let sol = Paragraph::new(sol_lines).alignment(Alignment::Center);
        frame.render_widget(sol, cols[1]);

        let footer_text = format!(
            "Also on GitHub Sponsors: {}   |   Repo: {}\nPress [Esc] to return to the main menu.",
            GITHUB_SPONSORS, GITHUB_REPO
        );
        let footer = Paragraph::new(footer_text)
            .style(Style::default().fg(Color::Blue))
            .alignment(Alignment::Center);
        frame.render_widget(footer, chunks[2]);
    }
}
