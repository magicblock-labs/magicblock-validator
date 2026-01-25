//! TUI state management

use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use tracing::Level;

/// Configuration for the TUI
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

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            rpc_url: "http://127.0.0.1:8899".to_string(),
            ws_url: "ws://127.0.0.1:8900".to_string(),
            remote_rpc_url: String::new(),
            validator_identity: String::new(),
            ledger_path: String::new(),
            block_time_ms: 400,
            lifecycle_mode: "ephemeral".to_string(),
            base_fee: 5000,
            help_url: "https://docs.magicblock.xyz".to_string(),
            version: String::new(),
            git_version: String::new(),
        }
    }
}

/// Static validator configuration for display in Config tab
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
        // Replace 0.0.0.0 with localhost for display
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

/// Active tab in the TUI
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

/// A single log entry
#[derive(Clone)]
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

/// A single transaction entry
#[derive(Clone)]
pub struct TransactionEntry {
    pub signature: String,
    pub slot: u64,
    pub success: bool,
    pub timestamp: DateTime<Utc>,
}

/// Detailed transaction information fetched from RPC
#[derive(Clone)]
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

/// View mode for the TUI
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    List,
    Detail,
}

/// The main TUI state
pub struct TuiState {
    /// Current slot number
    pub slot: u64,
    /// Current epoch number
    pub epoch: u64,
    /// Slots per epoch (for progress calculation)
    pub slots_per_epoch: u64,
    /// First slot of current epoch
    pub epoch_start_slot: u64,
    /// Ring buffer of log entries
    pub logs: VecDeque<LogEntry>,
    /// Maximum log entries to keep
    pub max_logs: usize,
    /// Ring buffer of transaction entries
    pub transactions: VecDeque<TransactionEntry>,
    /// Maximum transaction entries to keep
    pub max_transactions: usize,
    /// Currently active tab
    pub active_tab: Tab,
    /// Scroll offset for logs view
    pub log_scroll: usize,
    /// Scroll offset for transactions view
    pub tx_scroll: usize,
    /// Selected transaction index (for navigation)
    pub selected_tx: usize,
    /// Current view mode
    pub view_mode: ViewMode,
    /// Transaction detail being viewed
    pub tx_detail: Option<TransactionDetail>,
    /// Whether the TUI should quit
    pub should_quit: bool,
    /// Validator configuration for Config tab
    pub config: ValidatorConfig,
    /// RPC URL for fetching transaction details
    pub rpc_url: String,
    /// Full Solana Explorer URL for Config tab
    pub explorer_url: String,
    /// Help URL for footer
    pub help_url: String,
}

/// Simple URL encoding for the RPC URL
pub fn url_encode(s: &str) -> String {
    let mut encoded = String::with_capacity(s.len() * 3);
    for c in s.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => {
                encoded.push(c);
            }
            _ => {
                for byte in c.to_string().as_bytes() {
                    encoded.push_str(&format!("%{:02X}", byte));
                }
            }
        }
    }
    encoded
}

impl TuiState {
    pub fn new(config: TuiConfig) -> Self {
        // Build Solana Explorer URL with custom cluster
        // Replace 0.0.0.0 with localhost for the explorer URL
        let rpc_for_explorer = config.rpc_url.replace("0.0.0.0", "localhost");
        let encoded_rpc = url_encode(&rpc_for_explorer);
        let explorer_url =
            format!("https://explorer.solana.com/?cluster=custom&customUrl={encoded_rpc}");

        Self {
            slot: 0,
            epoch: 0,
            slots_per_epoch: 432_000, // Default Solana epoch length
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

    /// Update slot and calculate epoch
    pub fn update_slot(&mut self, slot: u64) {
        self.slot = slot;
        self.epoch = slot / self.slots_per_epoch;
        self.epoch_start_slot = self.epoch * self.slots_per_epoch;
    }

    /// Add a log entry
    pub fn push_log(&mut self, entry: LogEntry) {
        self.logs.push_front(entry);
        while self.logs.len() > self.max_logs {
            self.logs.pop_back();
        }
    }

    /// Add a transaction entry
    pub fn push_transaction(&mut self, entry: TransactionEntry) {
        self.transactions.push_front(entry);
        while self.transactions.len() > self.max_transactions {
            self.transactions.pop_back();
        }
    }

    /// Scroll up in the current tab
    pub fn scroll_up(&mut self) {
        match self.active_tab {
            Tab::Logs => {
                self.log_scroll = self.log_scroll.saturating_sub(1);
            }
            Tab::Transactions => {
                // Move selection up
                self.selected_tx = self.selected_tx.saturating_sub(1);
                // Auto-scroll to keep selection visible
                if self.selected_tx < self.tx_scroll {
                    self.tx_scroll = self.selected_tx;
                }
            }
            Tab::Config => {}
        }
    }

    /// Scroll down in the current tab
    pub fn scroll_down(&mut self, visible_height: usize) {
        match self.active_tab {
            Tab::Logs => {
                let max = self.logs.len().saturating_sub(visible_height);
                self.log_scroll = (self.log_scroll + 1).min(max);
            }
            Tab::Transactions => {
                // Move selection down
                if !self.transactions.is_empty() {
                    self.selected_tx =
                        (self.selected_tx + 1).min(self.transactions.len() - 1);
                    // Auto-scroll to keep selection visible
                    if self.selected_tx >= self.tx_scroll + visible_height {
                        self.tx_scroll = self.selected_tx - visible_height + 1;
                    }
                }
            }
            Tab::Config => {}
        }
    }

    /// Get the currently selected transaction signature
    pub fn selected_transaction(&self) -> Option<&TransactionEntry> {
        self.transactions.get(self.selected_tx)
    }

    /// Set transaction detail view
    pub fn show_tx_detail(&mut self, detail: TransactionDetail) {
        self.tx_detail = Some(detail);
        self.view_mode = ViewMode::Detail;
    }

    /// Close transaction detail view
    pub fn close_tx_detail(&mut self) {
        self.tx_detail = None;
        self.view_mode = ViewMode::List;
    }
}
