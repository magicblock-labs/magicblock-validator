//! TUI rendering and layout

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Tabs},
    Frame,
};
use tracing::Level;

use crate::state::{Tab, TuiState, ViewMode};

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

    // Render detail overlay if viewing transaction
    if state.view_mode == ViewMode::Detail {
        render_tx_detail_popup(frame, state);
    }
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
            Constraint::Min(10), // Progress bar (fills remaining space)
            Constraint::Length(slot_width), // Slot
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
            spans.push(Span::styled(
                "▌",
                Style::default().fg(Color::Rgb(40, 40, 40)),
            ));
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
        format!("Transactions ({})", tx_count),
        "Logs".to_string(),
        "Config".to_string(),
    ];

    let selected = match state.active_tab {
        Tab::Transactions => 0,
        Tab::Logs => 1,
        Tab::Config => 2,
    };

    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::BOTTOM))
        .select(selected)
        .style(Style::default().fg(DARK_GRAY))
        .highlight_style(
            Style::default().fg(WHITE).add_modifier(Modifier::BOLD),
        )
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
                Span::styled(
                    format!("{} ", timestamp),
                    Style::default().fg(DARK_GRAY),
                ),
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
        .enumerate()
        .skip(state.tx_scroll)
        .take(visible_count)
        .map(|(idx, tx)| {
            let timestamp = tx.timestamp.format("%H:%M:%S%.3f");
            let status_color =
                if tx.success { Color::Green } else { Color::Red };
            let status_char = if tx.success { "✓" } else { "✗" };
            let is_selected = idx == state.selected_tx;

            let line = Line::from(vec![
                Span::styled(
                    if is_selected { "▶ " } else { "  " },
                    Style::default().fg(CYAN),
                ),
                Span::styled(
                    format!("{} ", timestamp),
                    Style::default().fg(if is_selected {
                        WHITE
                    } else {
                        DARK_GRAY
                    }),
                ),
                Span::styled(status_char, Style::default().fg(status_color)),
                Span::raw(" "),
                Span::styled(
                    format!("Slot {} ", tx.slot),
                    Style::default().fg(if is_selected {
                        WHITE
                    } else {
                        DARK_GRAY
                    }),
                ),
                Span::styled(
                    &tx.signature,
                    Style::default().fg(if is_selected { CYAN } else { WHITE }),
                ),
            ]);

            let style = if is_selected {
                Style::default().bg(Color::Rgb(30, 30, 40))
            } else {
                Style::default()
            };

            ListItem::new(line).style(style)
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
            Span::styled(
                &config.validator_identity,
                Style::default().fg(WHITE),
            ),
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

    let paragraph = Paragraph::new(config_lines)
        .block(Block::default().borders(Borders::NONE));

    frame.render_widget(paragraph, area);
}

/// Render the footer with keyboard shortcuts and help link
fn render_footer(frame: &mut Frame, area: Rect, state: &TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(area);

    // Keyboard shortcuts - context aware
    let shortcuts = if state.view_mode == ViewMode::Detail {
        Line::from(vec![
            Span::styled("(Esc)", Style::default().fg(WHITE)),
            Span::styled(" close │ ", Style::default().fg(DARK_GRAY)),
            Span::styled("(↑↓)", Style::default().fg(WHITE)),
            Span::styled(" select │ ", Style::default().fg(DARK_GRAY)),
            Span::styled("(Enter)", Style::default().fg(WHITE)),
            Span::styled(" open in browser", Style::default().fg(DARK_GRAY)),
        ])
    } else if state.active_tab == Tab::Transactions {
        Line::from(vec![
            Span::styled("(Esc)", Style::default().fg(WHITE)),
            Span::styled(" quit │ ", Style::default().fg(DARK_GRAY)),
            Span::styled("(←→)", Style::default().fg(WHITE)),
            Span::styled(" tabs │ ", Style::default().fg(DARK_GRAY)),
            Span::styled("(↑↓)", Style::default().fg(WHITE)),
            Span::styled(" select │ ", Style::default().fg(DARK_GRAY)),
            Span::styled("(Enter)", Style::default().fg(WHITE)),
            Span::styled(" details", Style::default().fg(DARK_GRAY)),
        ])
    } else {
        Line::from(vec![
            Span::styled("(Esc)", Style::default().fg(WHITE)),
            Span::styled(" quit │ ", Style::default().fg(DARK_GRAY)),
            Span::styled("(←→)", Style::default().fg(WHITE)),
            Span::styled(" tabs │ ", Style::default().fg(DARK_GRAY)),
            Span::styled("(↑↓)", Style::default().fg(WHITE)),
            Span::styled(" scroll", Style::default().fg(DARK_GRAY)),
        ])
    };
    frame.render_widget(Paragraph::new(shortcuts), chunks[0]);

    // Help link
    let help = Paragraph::new(format!("Need help? {}", state.help_url))
        .style(Style::default().fg(DARK_GRAY))
        .alignment(Alignment::Right);
    frame.render_widget(help, chunks[1]);
}

/// Render the transaction detail popup
fn render_tx_detail_popup(frame: &mut Frame, state: &TuiState) {
    let Some(detail) = &state.tx_detail else {
        return;
    };

    let area = frame.area();
    // Create centered popup area (80% width, 80% height)
    let popup_width = (area.width * 80) / 100;
    let popup_height = (area.height * 80) / 100;
    let popup_x = (area.width - popup_width) / 2;
    let popup_y = (area.height - popup_height) / 2;
    let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

    // Clear the popup area with a background
    let clear =
        Block::default().style(Style::default().bg(Color::Rgb(20, 20, 30)));
    frame.render_widget(clear, popup_area);

    // Build the content
    let status_color = if detail.success {
        Color::Green
    } else {
        Color::Red
    };
    let status_text = if detail.success { "Success" } else { "Failed" };

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Signature: ", Style::default().fg(DARK_GRAY)),
            Span::styled(&detail.signature, Style::default().fg(CYAN)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Slot:      ", Style::default().fg(DARK_GRAY)),
            Span::styled(detail.slot.to_string(), Style::default().fg(WHITE)),
        ]),
        Line::from(vec![
            Span::styled("Status:    ", Style::default().fg(DARK_GRAY)),
            Span::styled(status_text, Style::default().fg(status_color)),
        ]),
        Line::from(vec![
            Span::styled("Fee:       ", Style::default().fg(DARK_GRAY)),
            Span::styled(
                format!("{} lamports", detail.fee),
                Style::default().fg(WHITE),
            ),
        ]),
    ];

    if let Some(cu) = detail.compute_units {
        lines.push(Line::from(vec![
            Span::styled("Compute:   ", Style::default().fg(DARK_GRAY)),
            Span::styled(format!("{} units", cu), Style::default().fg(WHITE)),
        ]));
    }

    // Add explorer link with selection indicator
    lines.push(Line::from(""));
    let explorer_style = if detail.explorer_selected {
        Style::default()
            .fg(CYAN)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::UNDERLINED)
    };
    lines.push(Line::from(vec![
        Span::styled(
            if detail.explorer_selected {
                "▶ "
            } else {
                "  "
            },
            Style::default().fg(CYAN),
        ),
        Span::styled("Explorer:  ", Style::default().fg(DARK_GRAY)),
        Span::styled(&detail.explorer_url, explorer_style),
    ]));

    if let Some(err) = &detail.error {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("Error:     ", Style::default().fg(DARK_GRAY)),
            Span::styled(err, Style::default().fg(Color::Red)),
        ]));
    }

    // Accounts section
    if !detail.accounts.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Accounts:",
            Style::default().fg(DARK_GRAY).add_modifier(Modifier::BOLD),
        )));
        for (i, acc) in detail.accounts.iter().take(10).enumerate() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  [{:2}] ", i),
                    Style::default().fg(DARK_GRAY),
                ),
                Span::styled(acc, Style::default().fg(WHITE)),
            ]));
        }
        if detail.accounts.len() > 10 {
            lines.push(Line::from(Span::styled(
                format!("  ... and {} more", detail.accounts.len() - 10),
                Style::default().fg(DARK_GRAY),
            )));
        }
    }

    // Logs section
    if !detail.logs.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Logs:",
            Style::default().fg(DARK_GRAY).add_modifier(Modifier::BOLD),
        )));
        let max_logs = (popup_height as usize).saturating_sub(lines.len() + 4);
        for log in detail.logs.iter().take(max_logs) {
            // Truncate long log lines
            let truncated = if log.len() > popup_width as usize - 4 {
                format!("{}...", &log[..popup_width as usize - 7])
            } else {
                log.clone()
            };
            lines.push(Line::from(Span::styled(
                format!("  {}", truncated),
                Style::default().fg(Color::Rgb(150, 150, 150)),
            )));
        }
        if detail.logs.len() > max_logs {
            lines.push(Line::from(Span::styled(
                format!(
                    "  ... and {} more lines",
                    detail.logs.len() - max_logs
                ),
                Style::default().fg(DARK_GRAY),
            )));
        }
    }

    let block = Block::default()
        .title(" Transaction Details ")
        .title_style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(CYAN))
        .style(Style::default().bg(Color::Rgb(20, 20, 30)));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, popup_area);
}
