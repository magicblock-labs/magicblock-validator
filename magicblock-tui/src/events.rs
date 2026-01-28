//! Event handling for TUI keyboard input

use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};

use crate::state::{Tab, TuiState, ViewMode};

/// Action to be taken after handling an event
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventAction {
    None,
    FetchTransaction(String),
    OpenUrl(String),
}

/// Poll for keyboard events with a timeout
pub fn poll_event(timeout: Duration) -> Option<Event> {
    if event::poll(timeout).ok()? {
        event::read().ok()
    } else {
        None
    }
}

/// Handle a keyboard event, updating state accordingly
/// Returns an action to be performed (e.g., fetch transaction)
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
    // Handle detail view mode separately
    if state.view_mode == ViewMode::Detail {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                state.close_tx_detail();
            }
            KeyCode::Enter => {
                // If explorer is selected, open the URL
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
                // Toggle explorer selection
                if let Some(detail) = &mut state.tx_detail {
                    detail.explorer_selected = !detail.explorer_selected;
                }
            }
            _ => {}
        }
        return EventAction::None;
    }

    // List view mode
    match key.code {
        // Quit
        KeyCode::Esc | KeyCode::Char('q') => {
            state.should_quit = true;
        }
        // Quit with Ctrl+C
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.should_quit = true;
        }
        // Enter to view transaction details
        KeyCode::Enter => {
            if state.active_tab == Tab::Transactions {
                if let Some(tx) = state.selected_transaction() {
                    return EventAction::FetchTransaction(tx.signature.clone());
                }
            }
        }
        // Tab navigation with left/right arrows
        KeyCode::Left | KeyCode::Char('h') => {
            state.active_tab = state.active_tab.prev();
        }
        KeyCode::Right | KeyCode::Char('l') => {
            state.active_tab = state.active_tab.next();
        }
        // Tab navigation with Tab key
        KeyCode::Tab => {
            state.active_tab = state.active_tab.next();
        }
        KeyCode::BackTab => {
            state.active_tab = state.active_tab.prev();
        }
        // Direct tab access
        KeyCode::Char('1') => {
            state.active_tab = Tab::Transactions;
        }
        KeyCode::Char('2') => {
            state.active_tab = Tab::Logs;
        }
        KeyCode::Char('3') => {
            state.active_tab = Tab::Config;
        }
        // Scrolling with up/down arrows
        KeyCode::Up | KeyCode::Char('k') => {
            state.scroll_up();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.scroll_down(visible_height);
        }
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
        KeyCode::Home => {
            state.log_scroll = 0;
            state.tx_scroll = 0;
            state.selected_tx = 0;
        }
        KeyCode::End => {
            // Scroll to bottom
            state.log_scroll = state.logs.len().saturating_sub(visible_height);
            if !state.transactions.is_empty() {
                state.selected_tx = state.transactions.len() - 1;
                state.tx_scroll =
                    state.transactions.len().saturating_sub(visible_height);
            }
        }
        _ => {}
    }
    EventAction::None
}
