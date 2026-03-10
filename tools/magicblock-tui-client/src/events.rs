use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};

use crate::{
    state::{Tab, TuiState, ViewMode},
    utils::url_encode,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventAction {
    None,
    FetchTransaction { rpc_url: String, signature: String },
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
            KeyCode::Char('c')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                state.close_tx_detail();
            }
            KeyCode::Enter => {
                if let Some(detail) = &state.tx_detail {
                    let url = match detail.selected_account_address() {
                        Some(account) => {
                            build_account_explorer_url(&detail.rpc_url, account)
                        }
                        None => detail.explorer_url.clone(),
                    };
                    state.close_tx_detail();
                    return EventAction::OpenUrl(url);
                }
                state.close_tx_detail();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(detail) = &mut state.tx_detail {
                    detail.move_selection_up();
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(detail) = &mut state.tx_detail {
                    detail.move_selection_down();
                }
            }
            _ => {}
        }
        return EventAction::None;
    }

    if matches!(
        key,
        KeyEvent {
            code: KeyCode::Esc,
            ..
        }
    ) {
        state.should_quit = true;
        return EventAction::None;
    }

    if matches!(
        key,
        KeyEvent {
            code: KeyCode::Char('c'),
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL)
    ) {
        state.should_quit = true;
        return EventAction::None;
    }

    if state.is_transaction_tab() {
        if key.modifiers.contains(KeyModifiers::ALT) {
            if let KeyCode::Char(ch) = key.code {
                if let Some(shortcut) = digit_shortcut(ch) {
                    state.select_tab_by_shortcut(shortcut);
                    return EventAction::None;
                }
            }
        }

        match key {
            KeyEvent {
                code: KeyCode::Backspace,
                modifiers,
                ..
            } if !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
            {
                state.pop_tx_filter_char();
                return EventAction::None;
            }
            KeyEvent {
                code: KeyCode::Char('u'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                state.clear_tx_filter();
                return EventAction::None;
            }
            KeyEvent {
                code: KeyCode::Char(ch),
                modifiers,
                ..
            } if !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
            {
                state.append_tx_filter_char(ch);
                return EventAction::None;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Char('q') => state.should_quit = true,
        KeyCode::Enter => {
            if state.is_transaction_tab() {
                if let (Some(tx), Some(rpc_url)) = (
                    state.selected_transaction(),
                    state.active_transaction_rpc_url(),
                ) {
                    return EventAction::FetchTransaction {
                        rpc_url: rpc_url.to_string(),
                        signature: tx.signature.clone(),
                    };
                }
            }
        }
        KeyCode::Left | KeyCode::Char('h') => {
            state.prev_tab();
        }
        KeyCode::Right | KeyCode::Char('l') => {
            state.next_tab();
        }
        KeyCode::Tab => {
            state.next_tab();
        }
        KeyCode::BackTab => {
            state.prev_tab();
        }
        KeyCode::Char('1') => state.select_tab_by_shortcut(1),
        KeyCode::Char('2') => state.select_tab_by_shortcut(2),
        KeyCode::Char('3') => state.select_tab_by_shortcut(3),
        KeyCode::Char('4') => state.select_tab_by_shortcut(4),
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
            Tab::Transactions | Tab::RemoteTransactions => {
                state.scroll_transactions_home();
            }
            Tab::Config => {}
        },
        KeyCode::End => match state.active_tab {
            Tab::Logs => {
                state.log_scroll =
                    state.logs.len().saturating_sub(visible_height);
            }
            Tab::Transactions | Tab::RemoteTransactions => {
                state.scroll_transactions_end(visible_height);
            }
            Tab::Config => {}
        },
        _ => {}
    }

    EventAction::None
}

fn digit_shortcut(ch: char) -> Option<u8> {
    match ch {
        '1' => Some(1),
        '2' => Some(2),
        '3' => Some(3),
        '4' => Some(4),
        _ => None,
    }
}

fn build_account_explorer_url(rpc_url: &str, account: &str) -> String {
    let encoded_rpc = url_encode(rpc_url);
    format!(
        "https://explorer.solana.com/address/{}?cluster=custom&customUrl={}",
        account, encoded_rpc
    )
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    use super::{handle_event, EventAction};
    use crate::state::{
        Tab, TransactionAccount, TransactionDetail, TuiConfig, TuiState,
        ViewMode,
    };

    fn config() -> TuiConfig {
        TuiConfig {
            rpc_url: "http://127.0.0.1:8899".to_string(),
            ws_url: "ws://127.0.0.1:8900".to_string(),
            remote_rpc_url: "https://api.devnet.solana.com".to_string(),
            validator_identity: "validator".to_string(),
            ledger_path: "/tmp/ledger".to_string(),
            block_time_ms: 400,
            lifecycle_mode: "ephemeral".to_string(),
            base_fee: 5_000,
            help_url: "https://example.com/help".to_string(),
            version: "1.0.0".to_string(),
            git_version: "abc123".to_string(),
        }
    }

    fn ctrl_c() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
    }

    fn account(pubkey: &str) -> TransactionAccount {
        TransactionAccount::new(pubkey, false, false)
    }

    #[test]
    fn ctrl_c_closes_detail_view() {
        let mut state = TuiState::new(config());
        state.show_tx_detail(TransactionDetail {
            signature: "sig".to_string(),
            slot: 1,
            success: true,
            fee: 0,
            compute_units: None,
            logs: vec![],
            accounts: vec![],
            error: None,
            rpc_url: "http://127.0.0.1:8899".to_string(),
            explorer_url: "https://example.com".to_string(),
            selected_account: None,
        });

        let action = handle_event(&mut state, ctrl_c(), 10);

        assert_eq!(action, EventAction::None);
        assert!(matches!(state.view_mode, ViewMode::List));
        assert!(state.tx_detail.is_none());
        assert!(!state.should_quit);
    }

    #[test]
    fn ctrl_c_quits_outside_detail_view() {
        let mut state = TuiState::new(config());
        assert!(matches!(state.view_mode, ViewMode::List));

        let _ = handle_event(&mut state, ctrl_c(), 10);

        assert!(state.should_quit);
    }

    #[test]
    fn enter_opens_selected_account_explorer() {
        let mut state = TuiState::new(config());
        state.show_tx_detail(TransactionDetail {
            signature: "sig".to_string(),
            slot: 1,
            success: true,
            fee: 0,
            compute_units: None,
            logs: vec![],
            accounts: vec![account("11111111111111111111111111111111")],
            error: None,
            rpc_url: "http://127.0.0.1:8898".to_string(),
            explorer_url: "https://explorer.solana.com/tx/sig".to_string(),
            selected_account: Some(0),
        });

        let action = handle_event(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            10,
        );

        match action {
            EventAction::OpenUrl(url) => {
                assert!(
                    url.contains("/address/11111111111111111111111111111111")
                );
                assert!(url
                    .contains("customUrl=http%3A%2F%2F127%2E0%2E0%2E1%3A8898"));
            }
            other => panic!("expected OpenUrl action, got {:?}", other),
        }
        assert!(matches!(state.view_mode, ViewMode::List));
        assert!(state.tx_detail.is_none());
    }

    #[test]
    fn detail_navigation_cycles_between_accounts_and_explorer() {
        let mut state = TuiState::new(config());
        state.show_tx_detail(TransactionDetail {
            signature: "sig".to_string(),
            slot: 1,
            success: true,
            fee: 0,
            compute_units: None,
            logs: vec![],
            accounts: vec![account("acc-1"), account("acc-2")],
            error: None,
            rpc_url: "http://127.0.0.1:8899".to_string(),
            explorer_url: "https://explorer.solana.com/tx/sig".to_string(),
            selected_account: Some(0),
        });

        let _ = handle_event(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            10,
        );
        assert_eq!(
            state.tx_detail.as_ref().and_then(|d| d.selected_account),
            Some(1)
        );

        let _ = handle_event(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            10,
        );
        assert_eq!(
            state.tx_detail.as_ref().and_then(|d| d.selected_account),
            None
        );

        let _ = handle_event(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            10,
        );
        assert_eq!(
            state.tx_detail.as_ref().and_then(|d| d.selected_account),
            Some(1)
        );
    }

    #[test]
    fn typing_in_transactions_updates_filter() {
        let mut state = TuiState::new(config());
        state.active_tab = Tab::Transactions;

        let _ = handle_event(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            10,
        );

        assert_eq!(state.tx_filter_query(), "q");
        assert!(!state.should_quit);
    }

    #[test]
    fn backspace_and_ctrl_u_edit_transaction_filter() {
        let mut state = TuiState::new(config());
        state.active_tab = Tab::Transactions;

        let _ = handle_event(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            10,
        );
        let _ = handle_event(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE)),
            10,
        );
        let _ = handle_event(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
            10,
        );
        assert_eq!(state.tx_filter_query(), "a");

        let _ = handle_event(
            &mut state,
            Event::Key(KeyEvent::new(
                KeyCode::Char('u'),
                KeyModifiers::CONTROL,
            )),
            10,
        );
        assert_eq!(state.tx_filter_query(), "");
    }

    #[test]
    fn digits_are_available_in_transaction_filter() {
        let mut state = TuiState::new(TuiConfig {
            remote_rpc_url: "http://127.0.0.1:8898".to_string(),
            ..config()
        });
        state.active_tab = Tab::Transactions;

        let _ = handle_event(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE)),
            10,
        );

        assert_eq!(state.active_tab, Tab::Transactions);
        assert_eq!(state.tx_filter_query(), "2");
    }

    #[test]
    fn alt_digit_shortcuts_switch_tabs_from_transaction_tabs() {
        let mut state = TuiState::new(TuiConfig {
            remote_rpc_url: "http://127.0.0.1:8898".to_string(),
            ..config()
        });
        state.active_tab = Tab::Transactions;

        let _ = handle_event(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::ALT)),
            10,
        );

        assert_eq!(state.active_tab, Tab::RemoteTransactions);
        assert_eq!(state.tx_filter_query(), "");
    }
}
