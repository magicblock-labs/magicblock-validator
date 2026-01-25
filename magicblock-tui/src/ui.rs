//! TUI rendering and layout

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Tabs},
    Frame,
};
use tracing::Level;

use crate::state::{Tab, TuiState};

/// Main color scheme
const CYAN: Color = Color::Cyan;
const GREEN: Color = Color::Green;
const DARK_GRAY: Color = Color::DarkGray;
const WHITE: Color = Color::White;

/// Render the entire TUI
pub fn render(frame: &mut Frame, state: &TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Length(3), // Tabs
            Constraint::Min(10),   // Content
            Constraint::Length(1), // Footer
        ])
        .split(frame.area());

    render_header(frame, chunks[0], state);
    render_tabs(frame, chunks[1], state);
    render_content(frame, chunks[2], state);
    render_footer(frame, chunks[3], state);
}

/// Render the header with slot progress bar and slot number
fn render_header(frame: &mut Frame, area: Rect, state: &TuiState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(CYAN))
        .style(Style::default());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Slot label width
    let slot_width: u16 = 18;

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(10),              // Progress bar (fills remaining space)
            Constraint::Length(slot_width),   // Slot
        ])
        .split(inner);

    // Calculate tick count based on available width (minus 2 for brackets)
    let tick_count = (chunks[0].width.saturating_sub(2)) as usize;

    // Circular tick progress bar - wraps every tick_count slots
    let filled_ticks = if tick_count > 0 {
        (state.slot as usize) % tick_count
    } else {
        0
    };
    let tick_bar = render_tick_bar(filled_ticks, tick_count);
    let progress = Paragraph::new(tick_bar);
    frame.render_widget(progress, chunks[0]);

    // Slot number
    let slot_text = format!("SLOT {}", state.slot);
    let slot = Paragraph::new(slot_text)
        .style(Style::default().fg(WHITE))
        .alignment(Alignment::Center);
    frame.render_widget(slot, chunks[1]);
}

/// Render a circular tick progress bar with spaced vertical bars
fn render_tick_bar(filled: usize, total: usize) -> Line<'static> {
    let mut spans = Vec::with_capacity(total * 2 + 2);

    // Opening bracket
    spans.push(Span::styled("[", Style::default().fg(DARK_GRAY)));

    // Ticks with spacing - using thick block character
    for i in 0..total {
        if i < filled {
            // Filled tick - bright cyan thick bar
            spans.push(Span::styled("▌", Style::default().fg(CYAN)));
        } else {
            // Empty tick - dim thick bar
            spans.push(Span::styled("▌", Style::default().fg(Color::Rgb(40, 40, 40))));
        }
    }

    // Closing bracket
    spans.push(Span::styled("]", Style::default().fg(DARK_GRAY)));

    Line::from(spans)
}

/// Render the tab bar
fn render_tabs(frame: &mut Frame, area: Rect, state: &TuiState) {
    let tx_count = state.transactions.len();
    let titles = vec![
        "Logs".to_string(),
        format!("Transactions ({})", tx_count),
        "Config".to_string(),
    ];

    let selected = match state.active_tab {
        Tab::Logs => 0,
        Tab::Transactions => 1,
        Tab::Config => 2,
    };

    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::BOTTOM))
        .select(selected)
        .style(Style::default().fg(DARK_GRAY))
        .highlight_style(Style::default().fg(WHITE).add_modifier(Modifier::BOLD))
        .divider("│");

    frame.render_widget(tabs, area);
}

/// Render the main content area based on active tab
fn render_content(frame: &mut Frame, area: Rect, state: &TuiState) {
    match state.active_tab {
        Tab::Logs => render_logs(frame, area, state),
        Tab::Transactions => render_transactions(frame, area, state),
        Tab::Config => render_config(frame, area, state),
    }
}

/// Render the logs panel
fn render_logs(frame: &mut Frame, area: Rect, state: &TuiState) {
    let visible_count = area.height.saturating_sub(2) as usize;

    let items: Vec<ListItem> = state
        .logs
        .iter()
        .skip(state.log_scroll)
        .take(visible_count)
        .map(|log| {
            let timestamp = log.timestamp.format("%H:%M:%S%.3f");
            let level_color = match log.level {
                Level::ERROR => Color::Red,
                Level::WARN => Color::Yellow,
                Level::INFO => Color::Green,
                Level::DEBUG => Color::Blue,
                Level::TRACE => Color::Magenta,
            };

            let line = Line::from(vec![
                Span::styled(format!("{} ", timestamp), Style::default().fg(DARK_GRAY)),
                Span::styled("●", Style::default().fg(level_color)),
                Span::raw(" "),
                Span::raw(&log.message),
            ]);

            ListItem::new(line)
        })
        .collect();

    let list = List::new(items).block(Block::default().borders(Borders::NONE));

    frame.render_widget(list, area);
}

/// Render the transactions panel
fn render_transactions(frame: &mut Frame, area: Rect, state: &TuiState) {
    let visible_count = area.height.saturating_sub(2) as usize;

    let items: Vec<ListItem> = state
        .transactions
        .iter()
        .skip(state.tx_scroll)
        .take(visible_count)
        .map(|tx| {
            let timestamp = tx.timestamp.format("%H:%M:%S%.3f");
            let status_color = if tx.success { Color::Green } else { Color::Red };
            let status_char = if tx.success { "✓" } else { "✗" };

            let line = Line::from(vec![
                Span::styled(format!("{} ", timestamp), Style::default().fg(DARK_GRAY)),
                Span::styled(status_char, Style::default().fg(status_color)),
                Span::raw(" "),
                Span::styled(format!("Slot {} ", tx.slot), Style::default().fg(DARK_GRAY)),
                Span::raw(&tx.signature),
            ]);

            ListItem::new(line)
        })
        .collect();

    let list = List::new(items).block(Block::default().borders(Borders::NONE));

    frame.render_widget(list, area);
}

/// Render the config panel
fn render_config(frame: &mut Frame, area: Rect, state: &TuiState) {
    let config = &state.config;

    let config_lines = vec![
        Line::from(vec![
            Span::styled("Version:           ", Style::default().fg(DARK_GRAY)),
            Span::styled(&config.version, Style::default().fg(CYAN)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("RPC Endpoint:      ", Style::default().fg(DARK_GRAY)),
            Span::styled(&config.rpc_endpoint, Style::default().fg(CYAN)),
        ]),
        Line::from(vec![
            Span::styled("WebSocket:         ", Style::default().fg(DARK_GRAY)),
            Span::styled(&config.ws_endpoint, Style::default().fg(CYAN)),
        ]),
        Line::from(vec![
            Span::styled("Explorer:          ", Style::default().fg(DARK_GRAY)),
            Span::styled(&state.explorer_url, Style::default().fg(CYAN)),
        ]),
        Line::from(vec![
            Span::styled("Remote RPC:        ", Style::default().fg(DARK_GRAY)),
            Span::styled(&config.remote_rpc, Style::default().fg(WHITE)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Validator ID:      ", Style::default().fg(DARK_GRAY)),
            Span::styled(&config.validator_identity, Style::default().fg(WHITE)),
        ]),
        Line::from(vec![
            Span::styled("Ledger Path:       ", Style::default().fg(DARK_GRAY)),
            Span::styled(&config.ledger_path, Style::default().fg(WHITE)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Block Time:        ", Style::default().fg(DARK_GRAY)),
            Span::styled(&config.block_time, Style::default().fg(GREEN)),
        ]),
        Line::from(vec![
            Span::styled("Lifecycle Mode:    ", Style::default().fg(DARK_GRAY)),
            Span::styled(&config.lifecycle_mode, Style::default().fg(GREEN)),
        ]),
        Line::from(vec![
            Span::styled("Base Fee:          ", Style::default().fg(DARK_GRAY)),
            Span::styled(
                format!("{} lamports", config.base_fee),
                Style::default().fg(GREEN),
            ),
        ]),
    ];

    let paragraph = Paragraph::new(config_lines).block(Block::default().borders(Borders::NONE));

    frame.render_widget(paragraph, area);
}

/// Render the footer with keyboard shortcuts and help link
fn render_footer(frame: &mut Frame, area: Rect, state: &TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    // Keyboard shortcuts
    let shortcuts = Paragraph::new(Line::from(vec![
        Span::styled("(Esc)", Style::default().fg(WHITE)),
        Span::styled(" quit │ ", Style::default().fg(DARK_GRAY)),
        Span::styled("(←→)", Style::default().fg(WHITE)),
        Span::styled(" tabs │ ", Style::default().fg(DARK_GRAY)),
        Span::styled("(↑↓)", Style::default().fg(WHITE)),
        Span::styled(" scroll", Style::default().fg(DARK_GRAY)),
    ]));
    frame.render_widget(shortcuts, chunks[0]);

    // Help link
    let help = Paragraph::new(format!("Need help? {}", state.help_url))
        .style(Style::default().fg(DARK_GRAY))
        .alignment(Alignment::Right);
    frame.render_widget(help, chunks[1]);
}

