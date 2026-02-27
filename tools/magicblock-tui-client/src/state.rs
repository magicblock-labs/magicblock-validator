use std::collections::VecDeque;

use chrono::{DateTime, Utc};
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
    pub timestamp: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct TransactionDetail {
    pub signature: String,
    pub slot: u64,
    pub success: bool,
    pub fee: u64,
    pub compute_units: Option<u64>,
    pub logs: Vec<String>,
    pub accounts: Vec<String>,
    pub error: Option<String>,
    pub explorer_url: String,
    pub explorer_selected: bool,
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
        let had_transactions = !self.transactions.is_empty();
        let anchored_to_latest = self.selected_tx == 0 && self.tx_scroll == 0;
        self.transactions.push_front(entry);

        // Keep selection on the same transaction when prepending a new one.
        if had_transactions && !anchored_to_latest {
            self.selected_tx = self
                .selected_tx
                .saturating_add(1)
                .min(self.transactions.len().saturating_sub(1));

            // When the user is already scrolled away from the top, keep the
            // viewport anchored to the same items.
            if self.tx_scroll > 0 {
                self.tx_scroll = self.tx_scroll.saturating_add(1);
            }
        }

        while self.transactions.len() > self.max_transactions {
            self.transactions.pop_back();
        }

        if self.transactions.is_empty() {
            self.selected_tx = 0;
            self.tx_scroll = 0;
            return;
        }

        let max_index = self.transactions.len() - 1;
        self.selected_tx = self.selected_tx.min(max_index);
        self.tx_scroll = self.tx_scroll.min(max_index);
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
                if !self.transactions.is_empty() {
                    self.selected_tx =
                        (self.selected_tx + 1).min(self.transactions.len() - 1);
                    if self.selected_tx >= self.tx_scroll + visible_height {
                        self.tx_scroll = self.selected_tx - visible_height + 1;
                    }
                }
            }
            Tab::Config => {}
        }
    }

    pub fn selected_transaction(&self) -> Option<&TransactionEntry> {
        self.transactions.get(self.selected_tx)
    }

    pub fn show_tx_detail(&mut self, detail: TransactionDetail) {
        self.tx_detail = Some(detail);
        self.view_mode = ViewMode::Detail;
    }

    pub fn close_tx_detail(&mut self) {
        self.tx_detail = None;
        self.view_mode = ViewMode::List;
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

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
        TransactionEntry {
            signature: signature.to_string(),
            slot: 1,
            success: true,
            timestamp: Utc::now(),
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
}
