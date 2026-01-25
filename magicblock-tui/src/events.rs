//! Event handling for TUI keyboard input

use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};

use crate::state::{Tab, TuiState};

/// Poll for keyboard events with a timeout
pub fn poll_event(timeout: Duration) -> Option<Event> {
    if event::poll(timeout).ok()? {
        event::read().ok()
    } else {
        None
    }
}

/// Handle a keyboard event, updating state accordingly
/// Returns the visible height hint for scrolling (if resize occurred)
pub fn handle_event(state: &mut TuiState, event: Event, visible_height: usize) {
    if let Event::Key(key) = event {
        handle_key(state, key, visible_height);
    }
}

fn handle_key(state: &mut TuiState, key: KeyEvent, visible_height: usize) {
    match key.code {
        // Quit
        KeyCode::Esc | KeyCode::Char('q') => {
            state.should_quit = true;
        }
        // Quit with Ctrl+C
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.should_quit = true;
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
            state.active_tab = Tab::Logs;
        }
        KeyCode::Char('2') => {
            state.active_tab = Tab::Transactions;
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
        }
        KeyCode::End => {
            // Scroll to bottom
            state.log_scroll = state.logs.len().saturating_sub(visible_height);
            state.tx_scroll = state.transactions.len().saturating_sub(visible_height);
        }
        _ => {}
    }
}
