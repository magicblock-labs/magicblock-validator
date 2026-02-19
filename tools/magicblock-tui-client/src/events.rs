use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};

use crate::state::{Tab, TuiState, ViewMode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventAction {
    None,
    FetchTransaction(String),
    OpenUrl(String),
}

pub fn poll_event(timeout: Duration) -> Option<Event> {
    if event::poll(timeout).ok()? {
        event::read().ok()
    } else {
        None
    }
}

pub fn handle_event(
    state: &mut TuiState,
    event: Event,
    visible_height: usize,
) -> EventAction {
    if let Event::Key(key) = event {
        handle_key(state, key, visible_height)
    } else {
        EventAction::None
    }
}

fn handle_key(
    state: &mut TuiState,
    key: KeyEvent,
    visible_height: usize,
) -> EventAction {
    if state.view_mode == ViewMode::Detail {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => state.close_tx_detail(),
            KeyCode::Enter => {
                if let Some(detail) = &state.tx_detail {
                    if detail.explorer_selected {
                        let url = detail.explorer_url.clone();
                        state.close_tx_detail();
                        return EventAction::OpenUrl(url);
                    }
                }
                state.close_tx_detail();
            }
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::Char('k')
            | KeyCode::Char('j') => {
                if let Some(detail) = &mut state.tx_detail {
                    detail.explorer_selected = !detail.explorer_selected;
                }
            }
            _ => {}
        }
        return EventAction::None;
    }

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => state.should_quit = true,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.should_quit = true;
        }
        KeyCode::Enter => {
            if state.active_tab == Tab::Transactions {
                if let Some(tx) = state.selected_transaction() {
                    return EventAction::FetchTransaction(tx.signature.clone());
                }
            }
        }
        KeyCode::Left | KeyCode::Char('h') => {
            state.active_tab = state.active_tab.prev();
        }
        KeyCode::Right | KeyCode::Char('l') => {
            state.active_tab = state.active_tab.next();
        }
        KeyCode::Tab => {
            state.active_tab = state.active_tab.next();
        }
        KeyCode::BackTab => {
            state.active_tab = state.active_tab.prev();
        }
        KeyCode::Char('1') => state.active_tab = Tab::Transactions,
        KeyCode::Char('2') => state.active_tab = Tab::Logs,
        KeyCode::Char('3') => state.active_tab = Tab::Config,
        KeyCode::Up | KeyCode::Char('k') => state.scroll_up(),
        KeyCode::Down | KeyCode::Char('j') => state.scroll_down(visible_height),
        KeyCode::PageUp => {
            for _ in 0..10 {
                state.scroll_up();
            }
        }
        KeyCode::PageDown => {
            for _ in 0..10 {
                state.scroll_down(visible_height);
            }
        }
        KeyCode::Home => match state.active_tab {
            Tab::Logs => state.log_scroll = 0,
            Tab::Transactions => {
                state.tx_scroll = 0;
                state.selected_tx = 0;
            }
            Tab::Config => {}
        },
        KeyCode::End => match state.active_tab {
            Tab::Logs => {
                state.log_scroll =
                    state.logs.len().saturating_sub(visible_height);
            }
            Tab::Transactions => {
                if !state.transactions.is_empty() {
                    state.selected_tx = state.transactions.len() - 1;
                    state.tx_scroll =
                        state.transactions.len().saturating_sub(visible_height);
                }
            }
            Tab::Config => {}
        },
        _ => {}
    }

    EventAction::None
}
