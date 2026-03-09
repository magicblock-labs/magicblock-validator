use std::collections::VecDeque;

use chrono::{DateTime, Local, Utc};
use tracing::Level;

use crate::utils::url_encode;

#[derive(Clone)]
pub struct TuiConfig {
    pub rpc_url: String,
    pub ws_url: String,
    pub remote_rpc_url: String,
    pub validator_identity: String,
    pub ledger_path: String,
    pub block_time_ms: u64,
    pub lifecycle_mode: String,
    pub base_fee: u64,
    pub help_url: String,
    pub version: String,
    pub git_version: String,
}

#[derive(Clone, Default)]
pub struct ValidatorConfig {
    pub version: String,
    pub rpc_endpoint: String,
    pub ws_endpoint: String,
    pub remote_rpc: String,
    pub validator_identity: String,
    pub ledger_path: String,
    pub block_time: String,
    pub lifecycle_mode: String,
    pub base_fee: u64,
}

impl From<&TuiConfig> for ValidatorConfig {
    fn from(config: &TuiConfig) -> Self {
        let rpc_endpoint = config.rpc_url.replace("0.0.0.0", "localhost");
        let ws_endpoint = config.ws_url.replace("0.0.0.0", "localhost");
        Self {
            version: format!(
                "{} (Git: {})",
                config.version, config.git_version
            ),
            rpc_endpoint,
            ws_endpoint,
            remote_rpc: config.remote_rpc_url.clone(),
            validator_identity: config.validator_identity.clone(),
            ledger_path: config.ledger_path.clone(),
            block_time: format!("{}ms", config.block_time_ms),
            lifecycle_mode: config.lifecycle_mode.clone(),
            base_fee: config.base_fee,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Transactions,
    Logs,
    Config,
}

impl Tab {
    pub fn next(self) -> Self {
        match self {
            Tab::Transactions => Tab::Logs,
            Tab::Logs => Tab::Config,
            Tab::Config => Tab::Transactions,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Tab::Transactions => Tab::Config,
            Tab::Logs => Tab::Transactions,
            Tab::Config => Tab::Logs,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: Level,
    pub target: String,
    pub message: String,
}

impl LogEntry {
    pub fn new(level: Level, target: String, message: String) -> Self {
        Self {
            timestamp: Utc::now(),
            level,
            target,
            message,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TransactionEntry {
    pub signature: String,
    pub slot: u64,
    pub success: bool,
    pub timestamp: DateTime<Local>,
    pub accounts: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct TransactionAccount {
    pub pubkey: String,
    pub is_signer: bool,
    pub is_writable: bool,
}

impl TransactionAccount {
    pub fn new(
        pubkey: impl Into<String>,
        is_signer: bool,
        is_writable: bool,
    ) -> Self {
        Self {
            pubkey: pubkey.into(),
            is_signer,
            is_writable,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TransactionDetail {
    pub signature: String,
    pub slot: u64,
    pub success: bool,
    pub fee: u64,
    pub compute_units: Option<u64>,
    pub logs: Vec<String>,
    pub accounts: Vec<TransactionAccount>,
    pub error: Option<String>,
    pub explorer_url: String,
    pub selected_account: Option<usize>,
}

pub const MAX_DETAIL_ACCOUNTS: usize = 10;

impl TransactionDetail {
    fn selectable_accounts_len(&self) -> usize {
        self.accounts.len().min(MAX_DETAIL_ACCOUNTS)
    }

    fn clamp_selection(&mut self) {
        let selectable = self.selectable_accounts_len();
        if self.selected_account.is_some_and(|idx| idx >= selectable) {
            self.selected_account = None;
        }
    }

    pub fn move_selection_up(&mut self) {
        self.clamp_selection();
        let selectable = self.selectable_accounts_len();
        self.selected_account = match self.selected_account {
            None if selectable > 0 => Some(selectable - 1),
            None => None,
            Some(0) => None,
            Some(idx) => Some(idx.saturating_sub(1)),
        };
    }

    pub fn move_selection_down(&mut self) {
        self.clamp_selection();
        let selectable = self.selectable_accounts_len();
        self.selected_account = match self.selected_account {
            None if selectable > 0 => Some(0),
            None => None,
            Some(idx) if idx + 1 < selectable => Some(idx + 1),
            Some(_) => None,
        };
    }

    pub fn selected_account_address(&self) -> Option<&str> {
        self.selected_account
            .and_then(|idx| self.accounts.get(idx))
            .map(|account| account.pubkey.as_str())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    List,
    Detail,
}

pub struct TuiState {
    pub slot: u64,
    pub epoch: u64,
    pub slots_per_epoch: u64,
    pub epoch_start_slot: u64,
    pub logs: VecDeque<LogEntry>,
    pub max_logs: usize,
    pub transactions: VecDeque<TransactionEntry>,
    pub max_transactions: usize,
    pub active_tab: Tab,
    pub log_scroll: usize,
    pub tx_scroll: usize,
    pub selected_tx: usize,
    tx_filter_query: String,
    pub view_mode: ViewMode,
    pub tx_detail: Option<TransactionDetail>,
    pub should_quit: bool,
    pub config: ValidatorConfig,
    pub rpc_url: String,
    pub explorer_url: String,
    pub help_url: String,
}

impl TuiState {
    pub fn new(config: TuiConfig) -> Self {
        let rpc_for_explorer = config.rpc_url.replace("0.0.0.0", "localhost");
        let encoded_rpc = url_encode(&rpc_for_explorer);
        let explorer_url =
            format!("https://explorer.solana.com/?cluster=custom&customUrl={encoded_rpc}");

        Self {
            slot: 0,
            epoch: 0,
            slots_per_epoch: 432_000,
            epoch_start_slot: 0,
            logs: VecDeque::with_capacity(1000),
            max_logs: 1000,
            transactions: VecDeque::with_capacity(500),
            max_transactions: 500,
            active_tab: Tab::Transactions,
            log_scroll: 0,
            tx_scroll: 0,
            selected_tx: 0,
            tx_filter_query: String::new(),
            view_mode: ViewMode::List,
            tx_detail: None,
            should_quit: false,
            rpc_url: rpc_for_explorer.clone(),
            explorer_url,
            help_url: config.help_url.clone(),
            config: ValidatorConfig::from(&config),
        }
    }

    pub fn update_slot(&mut self, slot: u64) {
        self.slot = slot;
        self.epoch = slot / self.slots_per_epoch;
        self.epoch_start_slot = self.epoch * self.slots_per_epoch;
    }

    pub fn push_log(&mut self, entry: LogEntry) {
        self.logs.push_front(entry);
        while self.logs.len() > self.max_logs {
            self.logs.pop_back();
        }
    }

    pub fn push_transaction(&mut self, entry: TransactionEntry) {
        let had_transactions = self.filtered_transactions_len() > 0;
        let anchored_to_latest = self.selected_tx == 0 && self.tx_scroll == 0;
        let selected_signature_before =
            self.selected_transaction().map(|tx| tx.signature.clone());
        let selected_tx_before = self.selected_tx;
        let tx_scroll_before = self.tx_scroll;
        self.transactions.push_front(entry);

        // Keep selection on the same transaction when prepending a new one.
        if had_transactions && !anchored_to_latest {
            if let Some(selected_signature) = selected_signature_before {
                let filtered_indices = self.filtered_transaction_indices();
                if let Some(new_selected) = self
                    .find_filtered_position_by_signature(
                        &filtered_indices,
                        &selected_signature,
                    )
                {
                    self.selected_tx = new_selected;

                    // Keep the viewport anchored to the same transaction rows
                    // when filtered entries are prepended at the top.
                    if tx_scroll_before > 0 {
                        if new_selected >= selected_tx_before {
                            self.tx_scroll = tx_scroll_before.saturating_add(
                                new_selected - selected_tx_before,
                            );
                        } else {
                            self.tx_scroll = tx_scroll_before.saturating_sub(
                                selected_tx_before - new_selected,
                            );
                        }
                    }
                }
            }
        }

        while self.transactions.len() > self.max_transactions {
            self.transactions.pop_back();
        }
        self.clamp_tx_selection();
    }

    pub fn scroll_up(&mut self) {
        match self.active_tab {
            Tab::Logs => {
                self.log_scroll = self.log_scroll.saturating_sub(1);
            }
            Tab::Transactions => {
                self.selected_tx = self.selected_tx.saturating_sub(1);
                if self.selected_tx < self.tx_scroll {
                    self.tx_scroll = self.selected_tx;
                }
            }
            Tab::Config => {}
        }
    }

    pub fn scroll_down(&mut self, visible_height: usize) {
        match self.active_tab {
            Tab::Logs => {
                let max = self.logs.len().saturating_sub(visible_height);
                self.log_scroll = (self.log_scroll + 1).min(max);
            }
            Tab::Transactions => {
                let tx_count = self.filtered_transactions_len();
                if tx_count > 0 {
                    self.selected_tx = (self.selected_tx + 1).min(tx_count - 1);
                    if self.selected_tx >= self.tx_scroll + visible_height {
                        self.tx_scroll = self.selected_tx - visible_height + 1;
                    }
                }
            }
            Tab::Config => {}
        }
    }

    pub fn selected_transaction(&self) -> Option<&TransactionEntry> {
        let tx_idx =
            *self.filtered_transaction_indices().get(self.selected_tx)?;
        self.transactions.get(tx_idx)
    }

    pub fn filtered_transactions(&self) -> Vec<&TransactionEntry> {
        self.transactions
            .iter()
            .filter(|tx| Self::tx_matches_filter(tx, &self.tx_filter_query))
            .collect()
    }

    pub fn filtered_transactions_len(&self) -> usize {
        self.transactions
            .iter()
            .filter(|tx| Self::tx_matches_filter(tx, &self.tx_filter_query))
            .count()
    }

    pub fn tx_filter_query(&self) -> &str {
        &self.tx_filter_query
    }

    pub fn append_tx_filter_char(&mut self, ch: char) {
        let selected_signature =
            self.selected_transaction().map(|tx| tx.signature.clone());
        self.tx_filter_query.push(ch);
        self.reconcile_selection(selected_signature.as_deref());
    }

    pub fn pop_tx_filter_char(&mut self) {
        let selected_signature =
            self.selected_transaction().map(|tx| tx.signature.clone());
        if self.tx_filter_query.pop().is_none() {
            return;
        }
        self.reconcile_selection(selected_signature.as_deref());
    }

    pub fn clear_tx_filter(&mut self) {
        if self.tx_filter_query.is_empty() {
            return;
        }
        let selected_signature =
            self.selected_transaction().map(|tx| tx.signature.clone());
        self.tx_filter_query.clear();
        self.reconcile_selection(selected_signature.as_deref());
    }

    pub fn show_tx_detail(&mut self, mut detail: TransactionDetail) {
        detail.clamp_selection();
        self.tx_detail = Some(detail);
        self.view_mode = ViewMode::Detail;
    }

    pub fn close_tx_detail(&mut self) {
        self.tx_detail = None;
        self.view_mode = ViewMode::List;
    }

    fn tx_matches_filter(tx: &TransactionEntry, filter: &str) -> bool {
        if filter.is_empty() {
            return true;
        }

        contains_ignore_ascii_case(&tx.signature, filter)
            || tx
                .accounts
                .iter()
                .any(|account| contains_ignore_ascii_case(account, filter))
    }

    fn filtered_transaction_indices(&self) -> Vec<usize> {
        self.transactions
            .iter()
            .enumerate()
            .filter_map(|(idx, tx)| {
                Self::tx_matches_filter(tx, &self.tx_filter_query)
                    .then_some(idx)
            })
            .collect()
    }

    fn find_filtered_position_by_signature(
        &self,
        filtered_indices: &[usize],
        signature: &str,
    ) -> Option<usize> {
        filtered_indices.iter().position(|idx| {
            self.transactions
                .get(*idx)
                .map(|tx| tx.signature.as_str() == signature)
                .unwrap_or(false)
        })
    }

    fn reconcile_selection(&mut self, selected_signature: Option<&str>) {
        let filtered_indices = self.filtered_transaction_indices();
        if filtered_indices.is_empty() {
            self.selected_tx = 0;
            self.tx_scroll = 0;
            return;
        }

        if let Some(selected_signature) = selected_signature {
            if let Some(new_selected) = self
                .find_filtered_position_by_signature(
                    &filtered_indices,
                    selected_signature,
                )
            {
                self.selected_tx = new_selected;
            }
        }

        self.clamp_tx_selection_with_len(filtered_indices.len());
    }

    fn clamp_tx_selection(&mut self) {
        let filtered_len = self.filtered_transactions_len();
        self.clamp_tx_selection_with_len(filtered_len);
    }

    fn clamp_tx_selection_with_len(&mut self, filtered_len: usize) {
        if filtered_len == 0 {
            self.selected_tx = 0;
            self.tx_scroll = 0;
            return;
        }

        let max_index = filtered_len - 1;
        self.selected_tx = self.selected_tx.min(max_index);
        self.tx_scroll = self.tx_scroll.min(max_index);

        if self.selected_tx < self.tx_scroll {
            self.tx_scroll = self.selected_tx;
        }
    }
}

fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }

    let needle_bytes = needle.as_bytes();
    haystack
        .as_bytes()
        .windows(needle_bytes.len())
        .any(|window| window.eq_ignore_ascii_case(needle_bytes))
}

#[cfg(test)]
mod tests {
    use chrono::Local;

    use super::{TransactionEntry, TuiConfig, TuiState};

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

    fn tx(signature: &str) -> TransactionEntry {
        tx_with_accounts(signature, vec![])
    }

    fn tx_with_accounts(
        signature: &str,
        accounts: Vec<&str>,
    ) -> TransactionEntry {
        TransactionEntry {
            signature: signature.to_string(),
            slot: 1,
            success: true,
            timestamp: Local::now(),
            accounts: accounts.into_iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn new_transactions_stay_visible_when_anchored_to_latest() {
        let mut state = TuiState::new(config());

        state.push_transaction(tx("sig-1"));
        state.push_transaction(tx("sig-2"));

        assert_eq!(state.selected_tx, 0);
        assert_eq!(state.tx_scroll, 0);
        assert_eq!(
            state.transactions.front().map(|t| t.signature.as_str()),
            Some("sig-2")
        );
    }

    #[test]
    fn prepending_keeps_selected_transaction_when_scrolled() {
        let mut state = TuiState::new(config());
        state.push_transaction(tx("sig-1"));
        state.push_transaction(tx("sig-2"));
        state.push_transaction(tx("sig-3"));

        state.selected_tx = 2;
        state.tx_scroll = 1;
        let selected_before = state
            .transactions
            .get(state.selected_tx)
            .expect("selected transaction should exist")
            .signature
            .clone();

        state.push_transaction(tx("sig-4"));

        assert_eq!(state.tx_scroll, 2);
        assert_eq!(
            state
                .transactions
                .get(state.selected_tx)
                .map(|t| &t.signature),
            Some(&selected_before)
        );
    }

    #[test]
    fn filter_matches_signature_or_account() {
        let mut state = TuiState::new(config());
        state.push_transaction(tx_with_accounts(
            "sig-aaa",
            vec!["Primary111111111111111111111111111111111111"],
        ));
        state.push_transaction(tx_with_accounts(
            "sig-bbb",
            vec!["Secondary22222222222222222222222222222222222"],
        ));

        for ch in "SIG-B".chars() {
            state.append_tx_filter_char(ch);
        }
        assert_eq!(state.filtered_transactions_len(), 1);
        assert_eq!(
            state.selected_transaction().map(|tx| tx.signature.as_str()),
            Some("sig-bbb")
        );

        state.clear_tx_filter();
        for ch in "primary11111".chars() {
            state.append_tx_filter_char(ch);
        }
        assert_eq!(state.filtered_transactions_len(), 1);
        assert_eq!(
            state.selected_transaction().map(|tx| tx.signature.as_str()),
            Some("sig-aaa")
        );
    }

    #[test]
    fn filter_with_no_matches_clears_selection() {
        let mut state = TuiState::new(config());
        state.push_transaction(tx("sig-aaa"));

        for ch in "does-not-exist".chars() {
            state.append_tx_filter_char(ch);
        }

        assert_eq!(state.filtered_transactions_len(), 0);
        assert!(state.selected_transaction().is_none());
        assert_eq!(state.selected_tx, 0);
        assert_eq!(state.tx_scroll, 0);
    }
}
