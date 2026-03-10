use std::collections::VecDeque;

use chrono::{DateTime, Local, Utc};
use tracing::Level;

use crate::utils::{is_localhost_http_url, url_encode};

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
            remote_rpc: config.remote_rpc_url.replace("0.0.0.0", "localhost"),
            validator_identity: config.validator_identity.clone(),
            ledger_path: config.ledger_path.clone(),
            block_time: format!("{}ms", config.block_time_ms),
            lifecycle_mode: config.lifecycle_mode.clone(),
            base_fee: config.base_fee,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Transactions,
    RemoteTransactions,
    Logs,
    Config,
}

impl Tab {
    pub fn next(self, has_remote_transactions: bool) -> Self {
        match self {
            Tab::Transactions if has_remote_transactions => {
                Tab::RemoteTransactions
            }
            Tab::Transactions => Tab::Logs,
            Tab::RemoteTransactions => Tab::Logs,
            Tab::Logs => Tab::Config,
            Tab::Config => Tab::Transactions,
        }
    }

    pub fn prev(self, has_remote_transactions: bool) -> Self {
        match self {
            Tab::Transactions => Tab::Config,
            Tab::RemoteTransactions => Tab::Transactions,
            Tab::Logs if has_remote_transactions => Tab::RemoteTransactions,
            Tab::Logs => Tab::Transactions,
            Tab::Config => Tab::Logs,
        }
    }

    pub fn is_transaction_tab(self) -> bool {
        matches!(self, Tab::Transactions | Tab::RemoteTransactions)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransactionSource {
    Local,
    Remote,
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
    pub rpc_url: String,
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

pub struct TransactionPaneState {
    transactions: VecDeque<TransactionEntry>,
    max_transactions: usize,
    tx_scroll: usize,
    selected_tx: usize,
    tx_filter_query: String,
}

impl TransactionPaneState {
    fn new(max_transactions: usize) -> Self {
        Self {
            transactions: VecDeque::with_capacity(max_transactions),
            max_transactions,
            tx_scroll: 0,
            selected_tx: 0,
            tx_filter_query: String::new(),
        }
    }

    fn len(&self) -> usize {
        self.transactions.len()
    }

    fn push_transaction(&mut self, entry: TransactionEntry) {
        let had_transactions = self.filtered_transactions_len() > 0;
        let anchored_to_latest = self.selected_tx == 0 && self.tx_scroll == 0;
        let selected_signature_before =
            self.selected_transaction().map(|tx| tx.signature.clone());
        let selected_tx_before = self.selected_tx;
        let tx_scroll_before = self.tx_scroll;
        self.transactions.push_front(entry);

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

    fn scroll_up(&mut self) {
        self.selected_tx = self.selected_tx.saturating_sub(1);
        if self.selected_tx < self.tx_scroll {
            self.tx_scroll = self.selected_tx;
        }
    }

    fn scroll_down(&mut self, visible_height: usize) {
        let visible_height = visible_height.max(1);
        let tx_count = self.filtered_transactions_len();
        if tx_count > 0 {
            self.selected_tx = (self.selected_tx + 1).min(tx_count - 1);
            if self.selected_tx >= self.tx_scroll + visible_height {
                self.tx_scroll = self.selected_tx - visible_height + 1;
            }
        }
    }

    fn scroll_home(&mut self) {
        self.tx_scroll = 0;
        self.selected_tx = 0;
    }

    fn scroll_end(&mut self, visible_height: usize) {
        let visible_height = visible_height.max(1);
        let filtered_len = self.filtered_transactions_len();
        if filtered_len > 0 {
            self.selected_tx = filtered_len - 1;
            self.tx_scroll = filtered_len.saturating_sub(visible_height);
        }
    }

    fn selected_transaction(&self) -> Option<&TransactionEntry> {
        let tx_idx =
            *self.filtered_transaction_indices().get(self.selected_tx)?;
        self.transactions.get(tx_idx)
    }

    fn filtered_transactions(&self) -> Vec<&TransactionEntry> {
        self.transactions
            .iter()
            .filter(|tx| Self::tx_matches_filter(tx, &self.tx_filter_query))
            .collect()
    }

    fn filtered_transactions_len(&self) -> usize {
        self.transactions
            .iter()
            .filter(|tx| Self::tx_matches_filter(tx, &self.tx_filter_query))
            .count()
    }

    fn tx_filter_query(&self) -> &str {
        &self.tx_filter_query
    }

    fn append_tx_filter_char(&mut self, ch: char) {
        let selected_signature =
            self.selected_transaction().map(|tx| tx.signature.clone());
        self.tx_filter_query.push(ch);
        self.reconcile_selection(selected_signature.as_deref());
    }

    fn pop_tx_filter_char(&mut self) {
        let selected_signature =
            self.selected_transaction().map(|tx| tx.signature.clone());
        if self.tx_filter_query.pop().is_none() {
            return;
        }
        self.reconcile_selection(selected_signature.as_deref());
    }

    fn clear_tx_filter(&mut self) {
        if self.tx_filter_query.is_empty() {
            return;
        }
        let selected_signature =
            self.selected_transaction().map(|tx| tx.signature.clone());
        self.tx_filter_query.clear();
        self.reconcile_selection(selected_signature.as_deref());
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
        self.clamp_tx_selection_with_len(self.filtered_transactions_len());
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

pub struct TuiState {
    pub slot: u64,
    pub epoch: u64,
    pub slots_per_epoch: u64,
    pub epoch_start_slot: u64,
    pub logs: VecDeque<LogEntry>,
    pub max_logs: usize,
    pub active_tab: Tab,
    pub log_scroll: usize,
    pub view_mode: ViewMode,
    pub tx_detail: Option<TransactionDetail>,
    pub should_quit: bool,
    pub config: ValidatorConfig,
    pub rpc_url: String,
    pub remote_rpc_url: Option<String>,
    pub explorer_url: String,
    pub help_url: String,
    local_transactions: TransactionPaneState,
    remote_transactions: Option<TransactionPaneState>,
}

impl TuiState {
    pub fn new(config: TuiConfig) -> Self {
        let rpc_for_explorer = config.rpc_url.replace("0.0.0.0", "localhost");
        let remote_rpc_for_explorer =
            config.remote_rpc_url.replace("0.0.0.0", "localhost");
        let encoded_rpc = url_encode(&rpc_for_explorer);
        let explorer_url = format!(
            "https://explorer.solana.com/?cluster=custom&customUrl={encoded_rpc}"
        );
        let remote_rpc_url = is_localhost_http_url(&remote_rpc_for_explorer)
            .then_some(remote_rpc_for_explorer);
        let max_transactions = 500;

        Self {
            slot: 0,
            epoch: 0,
            slots_per_epoch: 432_000,
            epoch_start_slot: 0,
            logs: VecDeque::with_capacity(1000),
            max_logs: 1000,
            active_tab: Tab::Transactions,
            log_scroll: 0,
            view_mode: ViewMode::List,
            tx_detail: None,
            should_quit: false,
            rpc_url: rpc_for_explorer.clone(),
            remote_rpc_url: remote_rpc_url.clone(),
            explorer_url,
            help_url: config.help_url.clone(),
            config: ValidatorConfig::from(&config),
            local_transactions: TransactionPaneState::new(max_transactions),
            remote_transactions: remote_rpc_url
                .map(|_| TransactionPaneState::new(max_transactions)),
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

    pub fn has_remote_transactions(&self) -> bool {
        self.remote_transactions.is_some()
    }

    pub fn is_transaction_tab(&self) -> bool {
        self.active_tab.is_transaction_tab()
    }

    pub fn next_tab(&mut self) {
        self.active_tab = self.active_tab.next(self.has_remote_transactions());
    }

    pub fn prev_tab(&mut self) {
        self.active_tab = self.active_tab.prev(self.has_remote_transactions());
    }

    pub fn select_tab_by_shortcut(&mut self, index: u8) {
        self.active_tab = match (index, self.has_remote_transactions()) {
            (1, _) => Tab::Transactions,
            (2, true) => Tab::RemoteTransactions,
            (2, false) => Tab::Logs,
            (3, true) => Tab::Logs,
            (3, false) => Tab::Config,
            (4, true) => Tab::Config,
            _ => self.active_tab,
        };
    }

    pub fn transaction_count(&self, source: TransactionSource) -> usize {
        self.transaction_pane(source)
            .map(TransactionPaneState::len)
            .unwrap_or(0)
    }

    pub fn filtered_transactions_len_for(
        &self,
        source: TransactionSource,
    ) -> usize {
        self.transaction_pane(source)
            .map(TransactionPaneState::filtered_transactions_len)
            .unwrap_or(0)
    }

    pub fn tx_filter_query_for(&self, source: TransactionSource) -> &str {
        self.transaction_pane(source)
            .map(TransactionPaneState::tx_filter_query)
            .unwrap_or("")
    }

    pub fn active_transaction_rpc_url(&self) -> Option<&str> {
        match self.active_tab {
            Tab::Transactions => Some(&self.rpc_url),
            Tab::RemoteTransactions => self.remote_rpc_url.as_deref(),
            Tab::Logs | Tab::Config => None,
        }
    }

    pub fn push_transaction(
        &mut self,
        source: TransactionSource,
        entry: TransactionEntry,
    ) {
        if let Some(pane) = self.transaction_pane_mut(source) {
            pane.push_transaction(entry);
        }
    }

    pub fn scroll_up(&mut self) {
        match self.active_tab {
            Tab::Logs => {
                self.log_scroll = self.log_scroll.saturating_sub(1);
            }
            Tab::Transactions | Tab::RemoteTransactions => self
                .active_transaction_pane_mut()
                .expect("transaction tab should have a pane")
                .scroll_up(),
            Tab::Config => {}
        }
    }

    pub fn scroll_down(&mut self, visible_height: usize) {
        match self.active_tab {
            Tab::Logs => {
                let max = self.logs.len().saturating_sub(visible_height);
                self.log_scroll = (self.log_scroll + 1).min(max);
            }
            Tab::Transactions | Tab::RemoteTransactions => self
                .active_transaction_pane_mut()
                .expect("transaction tab should have a pane")
                .scroll_down(visible_height),
            Tab::Config => {}
        }
    }

    pub fn scroll_transactions_home(&mut self) {
        if let Some(pane) = self.active_transaction_pane_mut() {
            pane.scroll_home();
        }
    }

    pub fn scroll_transactions_end(&mut self, visible_height: usize) {
        if let Some(pane) = self.active_transaction_pane_mut() {
            pane.scroll_end(visible_height);
        }
    }

    pub fn active_transaction_scroll(&self) -> usize {
        self.active_transaction_pane()
            .map(|pane| pane.tx_scroll)
            .unwrap_or(0)
    }

    pub fn active_transaction_selected(&self) -> usize {
        self.active_transaction_pane()
            .map(|pane| pane.selected_tx)
            .unwrap_or(0)
    }

    pub fn selected_transaction(&self) -> Option<&TransactionEntry> {
        self.active_transaction_pane()
            .and_then(TransactionPaneState::selected_transaction)
    }

    pub fn filtered_transactions(&self) -> Vec<&TransactionEntry> {
        self.active_transaction_pane()
            .map(TransactionPaneState::filtered_transactions)
            .unwrap_or_default()
    }

    pub fn tx_filter_query(&self) -> &str {
        self.active_transaction_pane()
            .map(TransactionPaneState::tx_filter_query)
            .unwrap_or("")
    }

    pub fn append_tx_filter_char(&mut self, ch: char) {
        if let Some(pane) = self.active_transaction_pane_mut() {
            pane.append_tx_filter_char(ch);
        }
    }

    pub fn pop_tx_filter_char(&mut self) {
        if let Some(pane) = self.active_transaction_pane_mut() {
            pane.pop_tx_filter_char();
        }
    }

    pub fn clear_tx_filter(&mut self) {
        if let Some(pane) = self.active_transaction_pane_mut() {
            pane.clear_tx_filter();
        }
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

    fn active_transaction_pane(&self) -> Option<&TransactionPaneState> {
        match self.active_tab {
            Tab::Transactions => Some(&self.local_transactions),
            Tab::RemoteTransactions => self.remote_transactions.as_ref(),
            Tab::Logs | Tab::Config => None,
        }
    }

    fn active_transaction_pane_mut(
        &mut self,
    ) -> Option<&mut TransactionPaneState> {
        match self.active_tab {
            Tab::Transactions => Some(&mut self.local_transactions),
            Tab::RemoteTransactions => self.remote_transactions.as_mut(),
            Tab::Logs | Tab::Config => None,
        }
    }

    fn transaction_pane(
        &self,
        source: TransactionSource,
    ) -> Option<&TransactionPaneState> {
        match source {
            TransactionSource::Local => Some(&self.local_transactions),
            TransactionSource::Remote => self.remote_transactions.as_ref(),
        }
    }

    fn transaction_pane_mut(
        &mut self,
        source: TransactionSource,
    ) -> Option<&mut TransactionPaneState> {
        match source {
            TransactionSource::Local => Some(&mut self.local_transactions),
            TransactionSource::Remote => self.remote_transactions.as_mut(),
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

    use super::{
        Tab, TransactionEntry, TransactionSource, TuiConfig, TuiState,
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

    fn remote_localhost_config() -> TuiConfig {
        TuiConfig {
            remote_rpc_url: "http://127.0.0.1:8898".to_string(),
            ..config()
        }
    }

    fn remote_localhost_ws_config() -> TuiConfig {
        TuiConfig {
            remote_rpc_url: "ws://127.0.0.1:8900".to_string(),
            ..config()
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

        state.push_transaction(TransactionSource::Local, tx("sig-1"));
        state.push_transaction(TransactionSource::Local, tx("sig-2"));

        assert_eq!(state.active_transaction_selected(), 0);
        assert_eq!(state.active_transaction_scroll(), 0);
        assert_eq!(
            state.selected_transaction().map(|t| t.signature.as_str()),
            Some("sig-2")
        );
    }

    #[test]
    fn prepending_keeps_selected_transaction_when_scrolled() {
        let mut state = TuiState::new(config());
        state.push_transaction(TransactionSource::Local, tx("sig-1"));
        state.push_transaction(TransactionSource::Local, tx("sig-2"));
        state.push_transaction(TransactionSource::Local, tx("sig-3"));

        state.scroll_down(1);
        state.scroll_down(1);
        state.scroll_up();
        let selected_before = state
            .selected_transaction()
            .expect("selected transaction should exist")
            .signature
            .to_string();

        state.push_transaction(TransactionSource::Local, tx("sig-4"));

        assert_eq!(state.active_transaction_selected(), 2);
        assert_eq!(state.active_transaction_scroll(), 2);
        assert_eq!(
            state.selected_transaction().map(|t| t.signature.as_str()),
            Some(selected_before.as_str())
        );
    }

    #[test]
    fn filter_matches_signature_or_account() {
        let mut state = TuiState::new(config());
        state.push_transaction(
            TransactionSource::Local,
            tx_with_accounts(
                "sig-aaa",
                vec!["Primary111111111111111111111111111111111111"],
            ),
        );
        state.push_transaction(
            TransactionSource::Local,
            tx_with_accounts(
                "sig-bbb",
                vec!["Secondary22222222222222222222222222222222222"],
            ),
        );

        for ch in "SIG-B".chars() {
            state.append_tx_filter_char(ch);
        }
        assert_eq!(state.filtered_transactions().len(), 1);
        assert_eq!(
            state.selected_transaction().map(|tx| tx.signature.as_str()),
            Some("sig-bbb")
        );

        state.clear_tx_filter();
        for ch in "primary11111".chars() {
            state.append_tx_filter_char(ch);
        }
        assert_eq!(state.filtered_transactions().len(), 1);
        assert_eq!(
            state.selected_transaction().map(|tx| tx.signature.as_str()),
            Some("sig-aaa")
        );
    }

    #[test]
    fn filter_with_no_matches_clears_selection() {
        let mut state = TuiState::new(config());
        state.push_transaction(TransactionSource::Local, tx("sig-aaa"));

        for ch in "does-not-exist".chars() {
            state.append_tx_filter_char(ch);
        }

        assert_eq!(state.filtered_transactions().len(), 0);
        assert!(state.selected_transaction().is_none());
        assert_eq!(state.active_transaction_selected(), 0);
        assert_eq!(state.active_transaction_scroll(), 0);
    }

    #[test]
    fn remote_transactions_tab_is_only_enabled_for_localhost_remote() {
        let state = TuiState::new(config());
        assert!(!state.has_remote_transactions());

        let remote_state = TuiState::new(remote_localhost_config());
        assert!(remote_state.has_remote_transactions());

        let ws_remote_state = TuiState::new(remote_localhost_ws_config());
        assert!(!ws_remote_state.has_remote_transactions());
    }

    #[test]
    fn remote_transactions_keep_independent_selection_and_filter() {
        let mut state = TuiState::new(remote_localhost_config());
        state.push_transaction(TransactionSource::Local, tx("local-1"));
        state.push_transaction(TransactionSource::Remote, tx("remote-1"));
        state.push_transaction(TransactionSource::Remote, tx("remote-2"));

        state.next_tab();
        assert_eq!(state.active_tab, Tab::RemoteTransactions);

        for ch in "remote-2".chars() {
            state.append_tx_filter_char(ch);
        }
        assert_eq!(state.filtered_transactions().len(), 1);
        assert_eq!(
            state.selected_transaction().map(|tx| tx.signature.as_str()),
            Some("remote-2")
        );

        state.prev_tab();
        assert_eq!(state.active_tab, Tab::Transactions);
        assert_eq!(state.tx_filter_query(), "");
        assert_eq!(
            state.selected_transaction().map(|tx| tx.signature.as_str()),
            Some("local-1")
        );
    }
}
