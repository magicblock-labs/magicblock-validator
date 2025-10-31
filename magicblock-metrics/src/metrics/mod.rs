use std::sync::Once;

pub use prometheus::HistogramTimer;
use prometheus::{
    Histogram, HistogramOpts, HistogramVec, IntCounter, IntCounterVec,
    IntGauge, IntGaugeVec, Opts, Registry,
};
pub use types::{AccountClone, AccountCommit, Outcome};
mod types;

// -----------------
// Buckets
// -----------------
// Prometheus collects durations in seconds
const MICROS_10_90: [f64; 9] = [
    0.000_01, 0.000_02, 0.000_03, 0.000_04, 0.000_05, 0.000_06, 0.000_07,
    0.000_08, 0.000_09,
];
const MICROS_100_900: [f64; 9] = [
    0.000_1, 0.000_2, 0.000_3, 0.000_4, 0.000_5, 0.000_6, 0.000_7, 0.000_8,
    0.000_9,
];
const MILLIS_1_9: [f64; 9] = [
    0.001, 0.002, 0.003, 0.004, 0.005, 0.006, 0.007, 0.008, 0.009,
];
const MILLIS_10_90: [f64; 9] =
    [0.01, 0.02, 0.03, 0.04, 0.05, 0.06, 0.07, 0.08, 0.09];
const MILLIS_100_900: [f64; 9] = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9];
const SECONDS_1_9: [f64; 9] = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0];

lazy_static::lazy_static! {
    pub (crate) static ref REGISTRY: Registry = Registry::new_custom(Some("mbv".to_string()), None).unwrap();

    static ref SLOT_COUNT: IntCounter = IntCounter::new(
        "slot_count", "Slot Count",
    ).unwrap();


    static ref CACHED_CLONE_OUTPUTS_COUNT: IntGauge = IntGauge::new(
        "magicblock_account_cloner_cached_outputs",
        "Number of cloned accounts in the RemoteAccountClonerWorker"
    )
    .unwrap();

    // -----------------
    // Ledger
    // -----------------
    static ref LEDGER_SIZE_GAUGE: IntGauge = IntGauge::new(
        "ledger_size", "Ledger size in Bytes",
    ).unwrap();
    static ref LEDGER_BLOCK_TIMES_GAUGE: IntGauge = IntGauge::new(
        "ledger_blocktimes_gauge", "Ledger Blocktimes Gauge",
    ).unwrap();
    static ref LEDGER_BLOCKHASHES_GAUGE: IntGauge = IntGauge::new(
        "ledger_blockhashes_gauge", "Ledger Blockhashes Gauge",
    ).unwrap();
    static ref LEDGER_SLOT_SIGNATURES_GAUGE: IntGauge = IntGauge::new(
        "ledger_slot_signatures_gauge", "Ledger Slot Signatures Gauge",
    ).unwrap();
    static ref LEDGER_ADDRESS_SIGNATURES_GAUGE: IntGauge = IntGauge::new(
        "ledger_address_signatures_gauge", "Ledger Address Signatures Gauge",
    ).unwrap();
    static ref LEDGER_TRANSACTION_STATUS_GAUGE: IntGauge = IntGauge::new(
        "ledger_transaction_status_gauge", "Ledger Transaction Status Gauge",
    ).unwrap();
    static ref LEDGER_TRANSACTION_SUCCESSFUL_STATUS_GAUGE: IntGauge = IntGauge::new(
        "ledger_transaction_successful_status_gauge", "Ledger Successful Transaction Status Gauge",
    ).unwrap();
    static ref LEDGER_TRANSACTION_FAILED_STATUS_GAUGE: IntGauge = IntGauge::new(
        "ledger_transaction_failed_status_gauge", "Ledger Failed Transaction Status Gauge",
    ).unwrap();
    static ref LEDGER_TRANSACTIONS_GAUGE: IntGauge = IntGauge::new(
        "ledger_transactions_gauge", "Ledger Transactions Gauge",
    ).unwrap();
    static ref LEDGER_TRANSACTION_MEMOS_GAUGE: IntGauge = IntGauge::new(
        "ledger_transaction_memos_gauge", "Ledger Transaction Memos Gauge",
    ).unwrap();
    static ref LEDGER_PERF_SAMPLES_GAUGE: IntGauge = IntGauge::new(
        "ledger_perf_samples_gauge", "Ledger Perf Samples Gauge",
    ).unwrap();
    static ref LEDGER_ACCOUNT_MOD_DATA_GAUGE: IntGauge = IntGauge::new(
        "ledger_account_mod_data_gauge", "Ledger Account Mod Data Gauge",
    ).unwrap();

    // -----------------
    // Accounts
    // -----------------
    static ref ACCOUNTS_SIZE_GAUGE: IntGauge = IntGauge::new(
        "accounts_size", "Size of persisted accounts (in bytes) currently on disk",
    ).unwrap();

    static ref ACCOUNTS_COUNT_GAUGE: IntGauge = IntGauge::new(
        "accounts_count", "Number of accounts currently in the database",
    ).unwrap();


    static ref PENDING_ACCOUNT_CLONES_GAUGE: IntGauge = IntGauge::new(
        "pending_account_clones", "Total number of account clone requests still in memory",
    ).unwrap();

    static ref MONITORED_ACCOUNTS_GAUGE: IntGauge = IntGauge::new(
        "monitored_accounts", "number of undelegated accounts, being monitored via websocket",
    ).unwrap();

    static ref EVICTED_ACCOUNTS_COUNT: IntGauge = IntGauge::new(
        "evicted_accounts", "number of accounts forcefully removed from monitored list and database",
    ).unwrap();

    // -----------------
    // RPC/Aperture
    // -----------------
    pub static ref ENSURE_ACCOUNTS_TIME: HistogramVec = HistogramVec::new(
        HistogramOpts::new("ensure_accounts_time", "Time spent in ensuring account presence")
            .buckets(
                MICROS_10_90.iter().chain(
                MICROS_100_900.iter()).chain(
                MILLIS_1_9.iter()).chain(
                MILLIS_10_90.iter()).chain(
                MILLIS_100_900.iter()).chain(
                SECONDS_1_9.iter()).cloned().collect()
            ),
        &["kind"]
    ).unwrap();

    pub static ref TRANSACTION_PROCESSING_TIME: Histogram = Histogram::with_opts(
        HistogramOpts::new("transaction_processing_time", "Total time spent in transaction processing")
            .buckets(
                MICROS_10_90.iter().chain(
                MICROS_100_900.iter()).chain(
                MILLIS_1_9.iter()).chain(
                MILLIS_10_90.iter()).chain(
                MILLIS_100_900.iter()).chain(
                SECONDS_1_9.iter()).cloned().collect()
            ),
    ).unwrap();

    pub static ref RPC_REQUEST_HANDLING_TIME: HistogramVec = HistogramVec::new(
        HistogramOpts::new("rpc_request_handling_time", "Time spent in rpc request handling")
            .buckets(
                MICROS_10_90.iter().chain(
                MICROS_100_900.iter()).chain(
                MILLIS_1_9.iter()).chain(
                MILLIS_10_90.iter()).chain(
                MILLIS_100_900.iter()).chain(
                SECONDS_1_9.iter()).cloned().collect()
            ), &["name"]
    ).unwrap();

    pub static ref TRANSACTION_SKIP_PREFLIGHT: IntCounter = IntCounter::new(
        "transaction_skip_preflight", "Count of transactions that skipped the preflight check",
    ).unwrap();

    pub static ref RPC_REQUESTS_COUNT: IntCounterVec = IntCounterVec::new(
        Opts::new("rpc_requests_count", "Count of different rpc requests"),
        &["name"],
    ).unwrap();

    pub static ref RPC_WS_SUBSCRIPTIONS_COUNT: IntGaugeVec = IntGaugeVec::new(
        Opts::new("rpc_ws_subscriptions_count", "Count of active rpc websocket subscriptions"),
        &["name"],
    ).unwrap();


    // -----------------
    // Transaction Execution
    // -----------------
    pub static ref FAILED_TRANSACTIONS_COUNT: IntCounter = IntCounter::new(
        "failed_transactions_count", "Total number of failed transactions"
    ).unwrap();


    // -----------------
    // CommittorService
    // -----------------
    static ref COMMITTOR_INTENTS_BACKLOG_COUNT: IntGauge = IntGauge::new(
        "committor_intent_backlog_count", "Number of intents in backlog",
    ).unwrap();

    static ref COMMITTOR_FAILED_INTENTS_COUNT: IntCounter = IntCounter::new(
        "committor_failed_intents_count", "Number of failed to be executed intents",
    ).unwrap();

    static ref COMMITTOR_EXECUTORS_BUSY_COUNT: IntGauge = IntGauge::new(
        "committor_executors_busy_count", "Number of busy intent executors"
    ).unwrap();
}

pub(crate) fn register() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        macro_rules! register {
            ($collector:ident) => {
                REGISTRY
                    .register(Box::new($collector.clone()))
                    .expect("collector can't be registered");
            };
        }
        register!(SLOT_COUNT);
        register!(CACHED_CLONE_OUTPUTS_COUNT);
        register!(LEDGER_SIZE_GAUGE);
        register!(LEDGER_BLOCK_TIMES_GAUGE);
        register!(LEDGER_BLOCKHASHES_GAUGE);
        register!(LEDGER_SLOT_SIGNATURES_GAUGE);
        register!(LEDGER_ADDRESS_SIGNATURES_GAUGE);
        register!(LEDGER_TRANSACTION_STATUS_GAUGE);
        register!(LEDGER_TRANSACTION_SUCCESSFUL_STATUS_GAUGE);
        register!(LEDGER_TRANSACTION_FAILED_STATUS_GAUGE);
        register!(LEDGER_TRANSACTIONS_GAUGE);
        register!(LEDGER_TRANSACTION_MEMOS_GAUGE);
        register!(LEDGER_PERF_SAMPLES_GAUGE);
        register!(LEDGER_ACCOUNT_MOD_DATA_GAUGE);
        register!(ACCOUNTS_SIZE_GAUGE);
        register!(ACCOUNTS_COUNT_GAUGE);
        register!(PENDING_ACCOUNT_CLONES_GAUGE);
        register!(MONITORED_ACCOUNTS_GAUGE);
        register!(EVICTED_ACCOUNTS_COUNT);
        register!(COMMITTOR_INTENTS_BACKLOG_COUNT);
        register!(COMMITTOR_FAILED_INTENTS_COUNT);
        register!(COMMITTOR_EXECUTORS_BUSY_COUNT);
        register!(ENSURE_ACCOUNTS_TIME);
        register!(RPC_REQUEST_HANDLING_TIME);
        register!(TRANSACTION_PROCESSING_TIME);
        register!(TRANSACTION_SKIP_PREFLIGHT);
        register!(RPC_REQUESTS_COUNT);
        register!(RPC_WS_SUBSCRIPTIONS_COUNT);
        register!(FAILED_TRANSACTIONS_COUNT);
    });
}

pub fn inc_slot() {
    SLOT_COUNT.inc();
}

pub fn set_cached_clone_outputs_count(count: usize) {
    CACHED_CLONE_OUTPUTS_COUNT.set(count as i64);
}

pub fn set_ledger_size(size: u64) {
    LEDGER_SIZE_GAUGE.set(size as i64);
}

pub fn set_ledger_block_times_count(count: i64) {
    LEDGER_BLOCK_TIMES_GAUGE.set(count);
}

pub fn set_ledger_blockhashes_count(count: i64) {
    LEDGER_BLOCKHASHES_GAUGE.set(count);
}

pub fn set_ledger_slot_signatures_count(count: i64) {
    LEDGER_SLOT_SIGNATURES_GAUGE.set(count);
}

pub fn set_ledger_address_signatures_count(count: i64) {
    LEDGER_ADDRESS_SIGNATURES_GAUGE.set(count);
}

pub fn set_ledger_transaction_status_count(count: i64) {
    LEDGER_TRANSACTION_STATUS_GAUGE.set(count);
}

pub fn set_ledger_transaction_successful_status_count(count: i64) {
    LEDGER_TRANSACTION_SUCCESSFUL_STATUS_GAUGE.set(count);
}

pub fn set_ledger_transaction_failed_status_count(count: i64) {
    LEDGER_TRANSACTION_FAILED_STATUS_GAUGE.set(count);
}

pub fn set_ledger_transactions_count(count: i64) {
    LEDGER_TRANSACTIONS_GAUGE.set(count);
}

pub fn set_ledger_transaction_memos_count(count: i64) {
    LEDGER_TRANSACTION_MEMOS_GAUGE.set(count);
}

pub fn set_ledger_perf_samples_count(count: i64) {
    LEDGER_PERF_SAMPLES_GAUGE.set(count);
}

pub fn set_ledger_account_mod_data_count(count: i64) {
    LEDGER_ACCOUNT_MOD_DATA_GAUGE.set(count);
}

pub fn inc_pending_clone_requests() {
    PENDING_ACCOUNT_CLONES_GAUGE.inc()
}

pub fn dec_pending_clone_requests() {
    PENDING_ACCOUNT_CLONES_GAUGE.dec()
}

pub fn ensure_accounts_end(timer: HistogramTimer) {
    timer.stop_and_record();
}

pub fn set_monitored_accounts_count(count: usize) {
    MONITORED_ACCOUNTS_GAUGE.set(count as i64);
}
pub fn inc_evicted_accounts_count() {
    EVICTED_ACCOUNTS_COUNT.inc();
}

pub fn set_committor_intents_backlog_count(value: i64) {
    COMMITTOR_INTENTS_BACKLOG_COUNT.set(value)
}

pub fn inc_committor_failed_intents_count() {
    COMMITTOR_FAILED_INTENTS_COUNT.inc()
}

pub fn set_committor_executors_busy_count(value: i64) {
    COMMITTOR_EXECUTORS_BUSY_COUNT.set(value)
}
