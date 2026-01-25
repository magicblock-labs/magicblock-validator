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
        Self {
            version: format!("{} (Git: {})", config.version, config.git_version),
            rpc_endpoint: config.rpc_url.clone(),
            ws_endpoint: config.ws_url.clone(),
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
    Logs,
    Transactions,
    Config,
}

impl Tab {
    pub fn next(self) -> Self {
        match self {
            Tab::Logs => Tab::Transactions,
            Tab::Transactions => Tab::Config,
            Tab::Config => Tab::Logs,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Tab::Logs => Tab::Config,
            Tab::Transactions => Tab::Logs,
            Tab::Config => Tab::Transactions,
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
    /// Whether the TUI should quit
    pub should_quit: bool,
    /// Validator configuration for Config tab
    pub config: ValidatorConfig,
    /// Short RPC URL for header display
    pub rpc_url: String,
    /// Full Solana Explorer URL for Config tab
    pub explorer_url: String,
    /// Help URL for footer
    pub help_url: String,
}

/// Simple URL encoding for the RPC URL
fn url_encode(s: &str) -> String {
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
            active_tab: Tab::Logs,
            log_scroll: 0,
            tx_scroll: 0,
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

    /// Get epoch progress as a ratio (0.0 to 1.0)
    pub fn epoch_progress(&self) -> f64 {
        if self.slots_per_epoch == 0 {
            return 0.0;
        }
        let slots_in_epoch = self.slot.saturating_sub(self.epoch_start_slot);
        (slots_in_epoch as f64 / self.slots_per_epoch as f64).min(1.0)
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
                self.tx_scroll = self.tx_scroll.saturating_sub(1);
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
                let max = self.transactions.len().saturating_sub(visible_height);
                self.tx_scroll = (self.tx_scroll + 1).min(max);
            }
            Tab::Config => {}
        }
    }
}
