use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs, Wrap},
    Frame,
};
use tracing::Level;

use crate::state::{
    Tab, TransactionAccount, TransactionDetail, TransactionSource, TuiState,
    ViewMode, MAX_DETAIL_ACCOUNTS,
};

const CYAN: Color = Color::Cyan;
const GREEN: Color = Color::Green;
const DARK_GRAY: Color = Color::DarkGray;
const WHITE: Color = Color::White;
const MIN_DETAIL_POPUP_WIDTH: u16 = 20;
const MIN_DETAIL_POPUP_HEIGHT: u16 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DetailPopupViewport {
    pub popup_area: Rect,
    pub inner_width: usize,
    pub inner_height: usize,
    pub max_scroll: usize,
}

pub fn render(frame: &mut Frame, state: &TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(1),
        ])
        .split(frame.area());

    render_header(frame, chunks[0], state);
    render_tabs(frame, chunks[1], state);
    render_content(frame, chunks[2], state);
    render_footer(frame, chunks[3], state);

    if state.view_mode == ViewMode::Detail {
        render_tx_detail_popup(frame, state);
    }
}

fn render_header(frame: &mut Frame, area: Rect, state: &TuiState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(CYAN))
        .style(Style::default());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let slot_width: u16 = 18;

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(10), Constraint::Length(slot_width)])
        .split(inner);

    let tick_count = (chunks[0].width.saturating_sub(2)) as usize;
    let filled_ticks = if tick_count > 0 {
        (state.slot as usize) % tick_count
    } else {
        0
    };
    let tick_bar = render_tick_bar(filled_ticks, tick_count);
    let progress = Paragraph::new(tick_bar);
    frame.render_widget(progress, chunks[0]);

    let slot_text = format!("SLOT {}", state.slot);
    let slot = Paragraph::new(slot_text)
        .style(Style::default().fg(WHITE))
        .alignment(Alignment::Center);
    frame.render_widget(slot, chunks[1]);
}

fn render_tick_bar(filled: usize, total: usize) -> Line<'static> {
    let mut spans = Vec::with_capacity(total + 2);
    spans.push(Span::styled("[", Style::default().fg(DARK_GRAY)));

    for i in 0..total {
        if i < filled {
            spans.push(Span::styled("▌", Style::default().fg(CYAN)));
        } else {
            spans.push(Span::styled(
                "▌",
                Style::default().fg(Color::Rgb(40, 40, 40)),
            ));
        }
    }

    spans.push(Span::styled("]", Style::default().fg(DARK_GRAY)));
    Line::from(spans)
}

fn render_tabs(frame: &mut Frame, area: Rect, state: &TuiState) {
    let local_tx_count = state.transaction_count(TransactionSource::Local);
    let local_filtered_tx_count =
        state.filtered_transactions_len_for(TransactionSource::Local);
    let tx_title = if state
        .tx_filter_query_for(TransactionSource::Local)
        .is_empty()
    {
        format!("Transactions ({})", local_tx_count)
    } else {
        format!(
            "Transactions ({}/{})",
            local_filtered_tx_count, local_tx_count
        )
    };
    let remote_tx_title = if state.has_remote_transactions() {
        let remote_tx_count =
            state.transaction_count(TransactionSource::Remote);
        let remote_filtered_tx_count =
            state.filtered_transactions_len_for(TransactionSource::Remote);
        if state
            .tx_filter_query_for(TransactionSource::Remote)
            .is_empty()
        {
            format!("Remote Transactions ({})", remote_tx_count)
        } else {
            format!(
                "Remote Transactions ({}/{})",
                remote_filtered_tx_count, remote_tx_count
            )
        }
    } else {
        String::new()
    };
    let mut titles = vec![tx_title];
    if state.has_remote_transactions() {
        titles.push(remote_tx_title);
    }
    titles.push("Logs".to_string());
    titles.push("Config".to_string());

    let selected = match (state.active_tab, state.has_remote_transactions()) {
        (Tab::Transactions, _) => 0,
        (Tab::RemoteTransactions, true) => 1,
        (Tab::Logs, true) => 2,
        (Tab::Config, true) => 3,
        (Tab::Logs, false) => 1,
        (Tab::Config, false) => 2,
        (Tab::RemoteTransactions, false) => 0,
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

fn render_content(frame: &mut Frame, area: Rect, state: &TuiState) {
    match state.active_tab {
        Tab::Logs => render_logs(frame, area, state),
        Tab::Transactions | Tab::RemoteTransactions => {
            render_transactions(frame, area, state)
        }
        Tab::Config => render_config(frame, area, state),
    }
}

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
                Span::styled(
                    format!("{}: ", log.target),
                    Style::default().fg(DARK_GRAY),
                ),
                Span::raw(&log.message),
            ]);

            ListItem::new(line)
        })
        .collect();

    let list = List::new(items).block(Block::default().borders(Borders::NONE));
    frame.render_widget(list, area);
}

fn render_transactions(frame: &mut Frame, area: Rect, state: &TuiState) {
    let tx_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    let filter_text = if state.tx_filter_query().is_empty() {
        "<type to filter by signature or account pubkey>"
    } else {
        state.tx_filter_query()
    };
    let filter_color = if state.tx_filter_query().is_empty() {
        DARK_GRAY
    } else {
        CYAN
    };
    let filter_line = Line::from(vec![
        Span::styled("Filter: ", Style::default().fg(DARK_GRAY)),
        Span::styled(filter_text, Style::default().fg(filter_color)),
    ]);
    frame.render_widget(Paragraph::new(filter_line), tx_chunks[0]);

    let visible_count = tx_chunks[1].height.saturating_sub(1) as usize;
    let filtered_transactions = state.filtered_transactions();

    let items: Vec<ListItem> = if filtered_transactions.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "No transactions match the current filter",
            Style::default().fg(DARK_GRAY),
        )))]
    } else {
        filtered_transactions
            .iter()
            .enumerate()
            .skip(state.active_transaction_scroll())
            .take(visible_count)
            .map(|(idx, tx)| {
                let timestamp = tx.timestamp.format("%H:%M:%S%.3f");
                let status_color =
                    if tx.success { Color::Green } else { Color::Red };
                let status_char = if tx.success { "✓" } else { "✗" };
                let is_selected = idx == state.active_transaction_selected();

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
                    Span::styled(
                        status_char,
                        Style::default().fg(status_color),
                    ),
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
                        Style::default().fg(if is_selected {
                            CYAN
                        } else {
                            WHITE
                        }),
                    ),
                ]);

                let style = if is_selected {
                    Style::default().bg(Color::Rgb(30, 30, 40))
                } else {
                    Style::default()
                };

                ListItem::new(line).style(style)
            })
            .collect()
    };

    let list = List::new(items).block(Block::default().borders(Borders::NONE));
    frame.render_widget(list, tx_chunks[1]);
}

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

fn render_footer(frame: &mut Frame, area: Rect, state: &TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(area);

    let shortcuts = if state.view_mode == ViewMode::Detail {
        Line::from(vec![
            Span::styled("(Esc)", Style::default().fg(WHITE)),
            Span::styled(" close │ ", Style::default().fg(DARK_GRAY)),
            Span::styled("(↑↓)", Style::default().fg(WHITE)),
            Span::styled(" select │ ", Style::default().fg(DARK_GRAY)),
            Span::styled("(PgUp/PgDn)", Style::default().fg(WHITE)),
            Span::styled(" scroll │ ", Style::default().fg(DARK_GRAY)),
            Span::styled("(Enter)", Style::default().fg(WHITE)),
            Span::styled(" open in browser", Style::default().fg(DARK_GRAY)),
        ])
    } else if state.is_transaction_tab() {
        Line::from(vec![
            Span::styled("(Esc)", Style::default().fg(WHITE)),
            Span::styled(" quit │ ", Style::default().fg(DARK_GRAY)),
            Span::styled("(←→)", Style::default().fg(WHITE)),
            Span::styled(" tabs │ ", Style::default().fg(DARK_GRAY)),
            Span::styled("(↑↓)", Style::default().fg(WHITE)),
            Span::styled(" select │ ", Style::default().fg(DARK_GRAY)),
            Span::styled("(Type)", Style::default().fg(WHITE)),
            Span::styled(" filter │ ", Style::default().fg(DARK_GRAY)),
            Span::styled("(Backspace)", Style::default().fg(WHITE)),
            Span::styled(" erase │ ", Style::default().fg(DARK_GRAY)),
            Span::styled("(Ctrl+U)", Style::default().fg(WHITE)),
            Span::styled(" clear │ ", Style::default().fg(DARK_GRAY)),
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

    let help = Paragraph::new(format!("Need help? {}", state.help_url))
        .style(Style::default().fg(DARK_GRAY))
        .alignment(Alignment::Right);
    frame.render_widget(help, chunks[1]);
}

fn render_tx_detail_popup(frame: &mut Frame, state: &TuiState) {
    let Some(detail) = &state.tx_detail else {
        return;
    };

    let Some(viewport) = detail_popup_viewport(detail, frame.area()) else {
        return;
    };

    // Hard-clear the popup rectangle to avoid stale characters from the
    // underlying panes when detail lines are shorter than the available width.
    frame.render_widget(Clear, viewport.popup_area);

    let block = Block::default()
        .title(" Transaction Details ")
        .title_style(Style::default().fg(CYAN).add_modifier(Modifier::BOLD))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(CYAN))
        .style(Style::default().bg(Color::Rgb(20, 20, 30)));

    let paragraph =
        Paragraph::new(build_tx_detail_lines(detail, viewport.inner_width))
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((
                detail
                    .detail_scroll
                    .min(viewport.max_scroll)
                    .min(u16::MAX as usize) as u16,
                0,
            ));
    frame.render_widget(paragraph, viewport.popup_area);
}

pub(crate) fn detail_popup_viewport_for_terminal(
    detail: &TransactionDetail,
    terminal_width: u16,
    terminal_height: u16,
) -> Option<DetailPopupViewport> {
    detail_popup_viewport(
        detail,
        Rect::new(0, 0, terminal_width, terminal_height),
    )
}

fn detail_popup_viewport(
    detail: &TransactionDetail,
    area: Rect,
) -> Option<DetailPopupViewport> {
    if area.width < MIN_DETAIL_POPUP_WIDTH
        || area.height < MIN_DETAIL_POPUP_HEIGHT
    {
        return None;
    }

    let popup_width = ((area.width * 80) / 100)
        .max(MIN_DETAIL_POPUP_WIDTH)
        .min(area.width);
    let popup_height = ((area.height * 80) / 100)
        .max(MIN_DETAIL_POPUP_HEIGHT)
        .min(area.height);
    let popup_x = area.width.saturating_sub(popup_width) / 2;
    let popup_y = area.height.saturating_sub(popup_height) / 2;
    let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);
    let inner_width = popup_width.saturating_sub(2) as usize;
    let inner_height = popup_height.saturating_sub(2) as usize;
    let lines = build_tx_detail_lines(detail, inner_width);
    let max_scroll = detail_scroll_max(&lines, inner_width, inner_height);

    Some(DetailPopupViewport {
        popup_area,
        inner_width,
        inner_height,
        max_scroll,
    })
}

fn build_tx_detail_lines(
    detail: &TransactionDetail,
    inner_width: usize,
) -> Vec<Line<'static>> {
    let status_color = if detail.success {
        Color::Green
    } else {
        Color::Red
    };
    let status_text = if detail.success { "Success" } else { "Failed" };
    let label_style = Style::default().fg(DARK_GRAY);

    let mut lines = vec![
        detail_field_line(
            "Signature: ",
            &detail.signature,
            Style::default().fg(CYAN),
        ),
        Line::from(""),
        detail_field_line(
            "Slot:      ",
            &detail.slot.to_string(),
            Style::default().fg(WHITE),
        ),
        detail_field_line(
            "Status:    ",
            status_text,
            Style::default().fg(status_color),
        ),
        detail_field_line(
            "Fee:       ",
            &format!("{} lamports", detail.fee),
            Style::default().fg(WHITE),
        ),
    ];

    if let Some(cu) = detail.compute_units {
        lines.push(detail_field_line(
            "Compute:   ",
            &format!("{} units", cu),
            Style::default().fg(WHITE),
        ));
    }

    lines.push(Line::from(""));
    let explorer_selected = detail.selected_account.is_none();
    let explorer_style = if explorer_selected {
        Style::default()
            .fg(CYAN)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::UNDERLINED)
    };
    let explorer_prefix =
        if explorer_selected { "▶ " } else { "  " }.to_string();
    let explorer_label = "Explorer:  ".to_string();
    lines.push(Line::from(vec![
        Span::styled(explorer_prefix, Style::default().fg(CYAN)),
        Span::styled(explorer_label, label_style),
        Span::styled(
            compact_explorer_url(&detail.explorer_url),
            explorer_style,
        ),
    ]));

    if let Some(err) = &detail.error {
        lines.push(Line::from(""));
        lines.push(detail_field_line(
            "Error:     ",
            err,
            Style::default().fg(Color::Red),
        ));
    }

    if !detail.accounts.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Accounts:",
            Style::default().fg(DARK_GRAY).add_modifier(Modifier::BOLD),
        )));
        for (i, acc) in
            detail.accounts.iter().take(MAX_DETAIL_ACCOUNTS).enumerate()
        {
            lines.push(detail_account_line(
                i,
                acc,
                detail.selected_account == Some(i),
                inner_width,
            ));
        }
        if detail.accounts.len() > MAX_DETAIL_ACCOUNTS {
            lines.push(Line::from(Span::styled(
                format!(
                    "  ... and {} more",
                    detail.accounts.len() - MAX_DETAIL_ACCOUNTS
                ),
                Style::default().fg(DARK_GRAY),
            )));
        }
    }

    if !detail.logs.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Logs:",
            Style::default().fg(DARK_GRAY).add_modifier(Modifier::BOLD),
        )));
        for log in &detail.logs {
            lines.push(Line::from(Span::styled(
                format!("  {}", sanitize_inline(log)),
                Style::default().fg(Color::Rgb(150, 150, 150)),
            )));
        }
    }

    lines
}

fn detail_scroll_max(
    lines: &[Line<'_>],
    inner_width: usize,
    inner_height: usize,
) -> usize {
    total_wrapped_height(lines, inner_width).saturating_sub(inner_height.max(1))
}

fn total_wrapped_height(lines: &[Line<'_>], inner_width: usize) -> usize {
    lines
        .iter()
        .map(|line| wrapped_line_height(line, inner_width))
        .sum()
}

fn wrapped_line_height(line: &Line<'_>, inner_width: usize) -> usize {
    let text = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    wrapped_text_height(&text, inner_width)
}

fn wrapped_text_height(text: &str, inner_width: usize) -> usize {
    let inner_width = inner_width.max(1);
    text.split('\n')
        .map(|segment| {
            let char_count = segment.chars().count();
            char_count.max(1).div_ceil(inner_width)
        })
        .sum()
}

fn detail_field_line(
    label: &str,
    value: &str,
    value_style: Style,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(label.to_string(), Style::default().fg(DARK_GRAY)),
        Span::styled(sanitize_inline(value), value_style),
    ])
}

fn detail_account_line(
    index: usize,
    account: &TransactionAccount,
    is_selected: bool,
    inner_width: usize,
) -> Line<'static> {
    let prefix = if is_selected {
        format!("▶ [{}] ", index)
    } else {
        format!("  [{}] ", index)
    };
    let marker_gap = 2;
    let marker_width = 3;
    let right_margin = 2;
    let pubkey_width = inner_width
        .saturating_sub(prefix.chars().count())
        .saturating_sub(marker_gap + marker_width + right_margin);
    let pubkey = pad_to_width(
        truncate_with_ellipsis(&account.pubkey, pubkey_width),
        pubkey_width,
    );
    let prefix_style = if is_selected {
        Style::default().fg(CYAN)
    } else {
        Style::default().fg(DARK_GRAY)
    };
    let pubkey_style = if is_selected {
        Style::default().fg(CYAN).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(WHITE)
    };
    let active_flag_style = if is_selected {
        Style::default().fg(CYAN).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(GREEN).add_modifier(Modifier::BOLD)
    };

    Line::from(vec![
        Span::styled(prefix, prefix_style),
        Span::styled(pubkey, pubkey_style),
        Span::raw("  "),
        Span::styled(
            if account.is_signer { "S" } else { " " },
            active_flag_style,
        ),
        Span::raw(" "),
        Span::styled(
            if account.is_writable { "W" } else { " " },
            active_flag_style,
        ),
        Span::raw(" ".repeat(right_margin)),
    ])
}

fn pad_to_width(value: String, width: usize) -> String {
    let padding = width.saturating_sub(value.chars().count());
    format!("{}{}", value, " ".repeat(padding))
}

fn sanitize_inline(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '\r' | '\n' | '\t' => ' ',
            _ => ch,
        })
        .collect()
}

fn compact_explorer_url(url: &str) -> String {
    let sanitized = sanitize_inline(url);
    let visible = sanitized
        .split_once('?')
        .map(|(base, _)| base)
        .unwrap_or(&sanitized);

    match visible.rsplit_once('/') {
        Some((prefix, tail)) if tail.chars().count() > 16 => format!(
            "{}/{}.....{}",
            prefix,
            first_chars(tail, 8),
            last_chars(tail, 8)
        ),
        _ => visible.to_string(),
    }
}

fn first_chars(value: &str, count: usize) -> String {
    value.chars().take(count).collect()
}

fn last_chars(value: &str, count: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    chars[chars.len().saturating_sub(count)..].iter().collect()
}

fn truncate_with_ellipsis(value: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let sanitized = sanitize_inline(value);

    if sanitized.chars().count() <= max_chars {
        return sanitized;
    }

    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }

    let truncate_at = sanitized
        .char_indices()
        .nth(max_chars - 3)
        .map(|(idx, _)| idx)
        .unwrap_or(sanitized.len());
    format!("{}...", &sanitized[..truncate_at])
}

#[cfg(test)]
mod tests {
    use super::{
        compact_explorer_url, detail_account_line,
        detail_popup_viewport_for_terminal, truncate_with_ellipsis,
    };
    use crate::state::{TransactionAccount, TransactionDetail};

    #[test]
    fn truncate_with_ellipsis_replaces_newlines() {
        assert_eq!(
            truncate_with_ellipsis("line1\nline2", 20),
            "line1 line2".to_string()
        );
    }

    #[test]
    fn truncate_with_ellipsis_shortens_overflow() {
        assert_eq!(
            truncate_with_ellipsis("abcdefghijklmnopqrstuvwxyz", 10),
            "abcdefg...".to_string()
        );
    }

    #[test]
    fn truncate_with_ellipsis_handles_tiny_widths() {
        assert_eq!(truncate_with_ellipsis("abcdef", 3), "...".to_string());
        assert_eq!(truncate_with_ellipsis("abcdef", 2), "..".to_string());
    }

    #[test]
    fn compact_explorer_url_drops_query_and_shortens_signature() {
        assert_eq!(
            compact_explorer_url(
                "https://explorer.solana.com/tx/z1szQZcd1234567890ABCDEFGH?cluster=custom&customUrl=http://127.0.0.1:7799"
            ),
            "https://explorer.solana.com/tx/z1szQZcd.....ABCDEFGH"
                .to_string()
        );
    }

    #[test]
    fn detail_account_line_aligns_signer_and_writable_markers() {
        let short = TransactionAccount::new("short", true, true);
        let long = TransactionAccount::new(
            "11111111111111111111111111111111",
            true,
            true,
        );

        let short_line: String = detail_account_line(0, &short, false, 48)
            .spans
            .into_iter()
            .map(|span| span.content.into_owned())
            .collect();
        let long_line: String = detail_account_line(1, &long, false, 48)
            .spans
            .into_iter()
            .map(|span| span.content.into_owned())
            .collect();

        assert_eq!(short_line.find('S'), long_line.find('S'));
        assert_eq!(short_line.find('W'), long_line.find('W'));
    }

    #[test]
    fn detail_popup_reports_scroll_for_wrapped_logs() {
        let detail = TransactionDetail {
            signature: "sig".to_string(),
            slot: 1,
            success: true,
            fee: 0,
            compute_units: None,
            logs: vec!["x".repeat(160); 12],
            accounts: vec![],
            error: None,
            rpc_url: "http://127.0.0.1:7799".to_string(),
            explorer_url: "https://explorer.solana.com/tx/sig".to_string(),
            selected_account: None,
            detail_scroll: 0,
        };

        let viewport =
            detail_popup_viewport_for_terminal(&detail, 120, 19).unwrap();

        assert!(viewport.max_scroll > 0);
    }
}
