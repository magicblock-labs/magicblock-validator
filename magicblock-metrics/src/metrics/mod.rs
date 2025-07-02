use std::sync::Once;

pub use prometheus::HistogramTimer;
use prometheus::{
    Histogram, HistogramOpts, IntCounter, IntCounterVec, IntGauge, IntGaugeVec,
    Opts, Registry,
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
const SECONDS_10_19: [f64; 10] =
    [10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 17.0, 18.0, 19.0];

lazy_static::lazy_static! {
    pub (crate) static ref REGISTRY: Registry = Registry::new_custom(Some("mbv".to_string()), None).unwrap();

    static ref SLOT_COUNT: IntCounter = IntCounter::new(
        "slot_count", "Slot Count",
    ).unwrap();

    static ref TRANSACTION_VEC_COUNT: IntCounterVec = IntCounterVec::new(
        Opts::new("transaction_count", "Transaction Count"),
        &["outcome"],
    ).unwrap();

    static ref FEE_PAYER_VEC_COUNT: IntCounterVec = IntCounterVec::new(
        Opts::new("fee_payer_count", "Count of transactions signed by specific fee payers"),
        &["fee_payer", "outcome"],
    ).unwrap();

    static ref EXECUTED_UNITS_COUNT: IntCounter = IntCounter::new(
        "executed_units_count", "Executed Units (CU) Count",
    ).unwrap();

    static ref FEE_COUNT: IntCounter = IntCounter::new(
        "fee_count", "Fee Count",
    ).unwrap();

    static ref ACCOUNT_CLONE_VEC_COUNT: IntCounterVec = IntCounterVec::new(
        Opts::new("account_clone_count", "Count clones performed for specific accounts"),
        &["kind", "pubkey", "owner"],
    ).unwrap();

    static ref ACCOUNT_COMMIT_VEC_COUNT: IntCounterVec = IntCounterVec::new(
        Opts::new("account_commit_count", "Count commits performed for specific accounts"),
        &["kind", "pubkey", "outcome"],
    ).unwrap();

    static ref ACCOUNT_COMMIT_TIME_HISTOGRAM: Histogram = Histogram::with_opts(
        HistogramOpts::new("account_commit_time", "Time until each account commit transaction is confirmed on chain")
            .buckets(
                MILLIS_10_90.iter().chain(
                MILLIS_100_900.iter()).chain(
                SECONDS_1_9.iter()).chain(
                SECONDS_10_19.iter()).cloned().collect()
            ),
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
    static ref LEDGER_BLOCK_TIMES_COUNT: IntCounter = IntCounter::new(
        "ledger_blocktimes_count", "Ledger Blocktimes Count",
    ).unwrap();
    static ref LEDGER_BLOCKHASHES_COUNT: IntCounter = IntCounter::new(
        "ledger_blockhashes_count", "Ledger Blockhashes Count",
    ).unwrap();
    static ref LEDGER_SLOT_SIGNATURES_COUNT: IntCounter = IntCounter::new(
        "ledger_slot_signatures_count", "Ledger Slot Signatures Count",
    ).unwrap();
    static ref LEDGER_ADDRESS_SIGNATURES_COUNT: IntCounter = IntCounter::new(
        "ledger_address_signatures_count", "Ledger Address Signatures Count",
    ).unwrap();
    static ref LEDGER_TRANSACTION_STATUS_COUNT: IntCounter = IntCounter::new(
        "ledger_transaction_status_count", "Ledger Transaction Status Count",
    ).unwrap();
    static ref LEDGER_TRANSACTION_SUCCESSFUL_STATUS_COUNT: IntCounter = IntCounter::new(
        "ledger_transaction_successful_status_count", "Ledger Successful Transaction Status Count",
    ).unwrap();
    static ref LEDGER_TRANSACTION_FAILED_STATUS_COUNT: IntCounter = IntCounter::new(
        "ledger_transaction_failed_status_count", "Ledger Failed Transaction Status Count",
    ).unwrap();
    static ref LEDGER_TRANSACTIONS_COUNT: IntCounter = IntCounter::new(
        "ledger_transactions_count", "Ledger Transactions Count",
    ).unwrap();
    static ref LEDGER_TRANSACTION_MEMOS_COUNT: IntCounter = IntCounter::new(
        "ledger_transaction_memos_count", "Ledger Transaction Memos Count",
    ).unwrap();
    static ref LEDGER_PERF_SAMPLES_COUNT: IntCounter = IntCounter::new(
        "ledger_perf_samples_count", "Ledger Perf Samples Count",
    ).unwrap();
    static ref LEDGER_ACCOUNT_MOD_DATA_COUNT: IntCounter = IntCounter::new(
        "ledger_account_mod_data_count", "Ledger Account Mod Data Count",
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

    static ref INMEM_ACCOUNTS_SIZE_GAUGE: IntGauge = IntGauge::new(
        "inmemory_accounts_size", "Size of account states kept in RAM",
    ).unwrap();

    static ref PENDING_ACCOUNT_CLONES_GAUGE: IntGauge = IntGauge::new(
        "pending_account_clones", "Total number of account clone requests still in memory",
    ).unwrap();

    static ref ACTIVE_DATA_MODS_GAUGE: IntGauge = IntGauge::new(
        "active_data_mods", "Total number of account data modifications held in memory",
    ).unwrap();

    static ref ACTIVE_DATA_MODS_SIZE_GAUGE: IntGauge = IntGauge::new(
        "active_data_mods_size", "Total memory consumption by account data modifications",
    ).unwrap();

    static ref SIGVERIFY_TIME_HISTOGRAM: Histogram = Histogram::with_opts(
        HistogramOpts::new("sigverify_time", "Time spent in sigverify")
            .buckets(
                MICROS_10_90.iter().chain(
                MICROS_100_900.iter()).chain(
                MILLIS_1_9.iter()).cloned().collect()
            ),
    ).unwrap();

    static ref ENSURE_ACCOUNTS_TIME_HISTOGRAM: Histogram = Histogram::with_opts(
        HistogramOpts::new("ensure_accounts_time", "Time spent in ensure_accounts")
            .buckets(
                MILLIS_1_9.iter().chain(
                MILLIS_10_90.iter()).chain(
                MILLIS_100_900.iter()).chain(
                SECONDS_1_9.iter()).cloned().collect()
            ),
    ).unwrap();

    static ref TRANSACTION_EXECUTION_TIME_HISTORY: Histogram = Histogram::with_opts(
        HistogramOpts::new("transaction_execution_time", "Time spent in transaction execution")
            .buckets(
                MICROS_10_90.iter().chain(
                MICROS_100_900.iter()).chain(
                MILLIS_1_9.iter()).cloned().collect()
            ),
    ).unwrap();

    static ref FLUSH_ACCOUNTS_TIME_HISTOGRAM: Histogram = Histogram::with_opts(
        HistogramOpts::new("flush_accounts_time", "Time spent flushing accounts to disk")
            .buckets(
                MILLIS_1_9.iter().chain(
                MILLIS_10_90.iter()).chain(
                MILLIS_100_900.iter()).chain(
                SECONDS_1_9.iter()).cloned().collect()
            ),
    ).unwrap();

    static ref MONITORED_ACCOUNTS_GAUGE: IntGauge = IntGauge::new(
        "monitored_accounts", "number of undelegated accounts, being monitored via websocket",
    ).unwrap();

    static ref SUBSCRIPTIONS_COUNT_GAUGE: IntGaugeVec = IntGaugeVec::new(
        Opts::new("subscriptions_count", "number of active account subscriptions"),
        &["shard"],
    ).unwrap();

    static ref EVICTED_ACCOUNTS_COUNT: IntGauge = IntGauge::new(
        "evicted_accounts", "number of accounts forcefully removed from monitored list and database",
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
        register!(TRANSACTION_VEC_COUNT);
        register!(FEE_PAYER_VEC_COUNT);
        register!(EXECUTED_UNITS_COUNT);
        register!(FEE_COUNT);
        register!(ACCOUNT_CLONE_VEC_COUNT);
        register!(ACCOUNT_COMMIT_VEC_COUNT);
        register!(ACCOUNT_COMMIT_TIME_HISTOGRAM);
        register!(CACHED_CLONE_OUTPUTS_COUNT);
        register!(LEDGER_SIZE_GAUGE);
        register!(LEDGER_BLOCK_TIMES_COUNT);
        register!(LEDGER_BLOCKHASHES_COUNT);
        register!(LEDGER_SLOT_SIGNATURES_COUNT);
        register!(LEDGER_ADDRESS_SIGNATURES_COUNT);
        register!(LEDGER_TRANSACTION_STATUS_COUNT);
        register!(LEDGER_TRANSACTION_SUCCESSFUL_STATUS_COUNT);
        register!(LEDGER_TRANSACTION_FAILED_STATUS_COUNT);
        register!(LEDGER_TRANSACTIONS_COUNT);
        register!(LEDGER_TRANSACTION_MEMOS_COUNT);
        register!(LEDGER_PERF_SAMPLES_COUNT);
        register!(LEDGER_ACCOUNT_MOD_DATA_COUNT);
        register!(ACCOUNTS_SIZE_GAUGE);
        register!(ACCOUNTS_COUNT_GAUGE);
        register!(INMEM_ACCOUNTS_SIZE_GAUGE);
        register!(PENDING_ACCOUNT_CLONES_GAUGE);
        register!(ACTIVE_DATA_MODS_GAUGE);
        register!(ACTIVE_DATA_MODS_SIZE_GAUGE);
        register!(SIGVERIFY_TIME_HISTOGRAM);
        register!(ENSURE_ACCOUNTS_TIME_HISTOGRAM);
        register!(TRANSACTION_EXECUTION_TIME_HISTORY);
        register!(FLUSH_ACCOUNTS_TIME_HISTOGRAM);
        register!(MONITORED_ACCOUNTS_GAUGE);
        register!(SUBSCRIPTIONS_COUNT_GAUGE);
        register!(EVICTED_ACCOUNTS_COUNT);
    });
}

pub fn inc_slot() {
    SLOT_COUNT.inc();
}

pub fn inc_transaction(is_ok: bool, fee_payer: &str) {
    let outcome = if is_ok { "success" } else { "error" };
    TRANSACTION_VEC_COUNT.with_label_values(&[outcome]).inc();
    FEE_PAYER_VEC_COUNT
        .with_label_values(&[fee_payer, outcome])
        .inc();
}

pub fn inc_executed_units(executed_units: u64) {
    EXECUTED_UNITS_COUNT.inc_by(executed_units);
}

pub fn inc_fee(fee: u64) {
    FEE_COUNT.inc_by(fee);
}

pub fn inc_account_clone(account_clone: AccountClone) {
    use AccountClone::*;
    match account_clone {
        FeePayer {
            pubkey,
            balance_pda,
        } => {
            ACCOUNT_CLONE_VEC_COUNT
                .with_label_values(&[
                    "feepayer",
                    pubkey,
                    balance_pda.unwrap_or(""),
                ])
                .inc();
        }
        Undelegated { pubkey, owner } => {
            ACCOUNT_CLONE_VEC_COUNT
                .with_label_values(&["undelegated", pubkey, owner])
                .inc();
        }
        Delegated { pubkey, owner } => {
            ACCOUNT_CLONE_VEC_COUNT
                .with_label_values(&["delegated", pubkey, owner])
                .inc();
        }
        Program { pubkey } => {
            ACCOUNT_CLONE_VEC_COUNT
                .with_label_values(&["program", pubkey, ""])
                .inc();
        }
    }
}

pub fn inc_account_commit(account_commit: AccountCommit) {
    use AccountCommit::*;
    match account_commit {
        CommitOnly { pubkey, outcome } => {
            ACCOUNT_COMMIT_VEC_COUNT
                .with_label_values(&["commit", pubkey, outcome.as_str()])
                .inc();
        }
        CommitAndUndelegate { pubkey, outcome } => {
            ACCOUNT_COMMIT_VEC_COUNT
                .with_label_values(&[
                    "commit_and_undelegate",
                    pubkey,
                    outcome.as_str(),
                ])
                .inc();
        }
    }
}

pub fn account_commit_start() -> HistogramTimer {
    ACCOUNT_COMMIT_TIME_HISTOGRAM.start_timer()
}

pub fn set_cached_clone_outputs_count(count: usize) {
    CACHED_CLONE_OUTPUTS_COUNT.set(count as i64);
}

pub fn account_commit_end(timer: HistogramTimer) {
    timer.stop_and_record();
}

pub fn set_subscriptions_count(count: usize, shard: &str) {
    SUBSCRIPTIONS_COUNT_GAUGE
        .with_label_values(&[shard])
        .set(count as i64);
}

pub fn set_ledger_size(size: u64) {
    LEDGER_SIZE_GAUGE.set(size as i64);
}

pub fn inc_ledger_block_times_count() {
    LEDGER_BLOCK_TIMES_COUNT.inc();
}

pub fn inc_ledger_blockhashes_count() {
    LEDGER_BLOCKHASHES_COUNT.inc();
}

pub fn inc_ledger_slot_signatures_count() {
    LEDGER_SLOT_SIGNATURES_COUNT.inc();
}

pub fn inc_ledger_address_signatures_count() {
    LEDGER_ADDRESS_SIGNATURES_COUNT.inc();
}

pub fn inc_ledger_transaction_status_count() {
    LEDGER_TRANSACTION_STATUS_COUNT.inc();
}

pub fn inc_ledger_transaction_successful_status_count() {
    LEDGER_TRANSACTION_SUCCESSFUL_STATUS_COUNT.inc();
}

pub fn inc_ledger_transaction_failed_status_count() {
    LEDGER_TRANSACTION_FAILED_STATUS_COUNT.inc();
}

pub fn inc_ledger_transactions_count() {
    LEDGER_TRANSACTIONS_COUNT.inc();
}

pub fn inc_ledger_transaction_memos_count() {
    LEDGER_TRANSACTION_MEMOS_COUNT.inc();
}

pub fn inc_ledger_perf_samples_count() {
    LEDGER_PERF_SAMPLES_COUNT.inc();
}

pub fn inc_ledger_account_mod_data_count() {
    LEDGER_ACCOUNT_MOD_DATA_COUNT.inc();
}

pub fn set_accounts_size(size: u64) {
    ACCOUNTS_SIZE_GAUGE.set(size as i64);
}

pub fn set_accounts_count(count: usize) {
    ACCOUNTS_COUNT_GAUGE.set(count as i64);
}

pub fn adjust_inmemory_accounts_size(delta: i64) {
    INMEM_ACCOUNTS_SIZE_GAUGE.add(delta);
}

pub fn inc_pending_clone_requests() {
    PENDING_ACCOUNT_CLONES_GAUGE.inc()
}

pub fn dec_pending_clone_requests() {
    PENDING_ACCOUNT_CLONES_GAUGE.dec()
}

pub fn adjust_active_data_mods(delta: i64) {
    ACTIVE_DATA_MODS_GAUGE.add(delta)
}

pub fn adjust_active_data_mods_size(delta: i64) {
    ACTIVE_DATA_MODS_SIZE_GAUGE.add(delta);
}

pub fn observe_sigverify_time<T, F>(f: F) -> T
where
    F: FnOnce() -> T,
{
    SIGVERIFY_TIME_HISTOGRAM.observe_closure_duration(f)
}

pub fn ensure_accounts_start() -> HistogramTimer {
    ENSURE_ACCOUNTS_TIME_HISTOGRAM.start_timer()
}

pub fn ensure_accounts_end(timer: HistogramTimer) {
    timer.stop_and_record();
}

pub fn observe_transaction_execution_time<T, F>(f: F) -> T
where
    F: FnOnce() -> T,
{
    TRANSACTION_EXECUTION_TIME_HISTORY.observe_closure_duration(f)
}

pub fn adjust_monitored_accounts_count(count: usize) {
    MONITORED_ACCOUNTS_GAUGE.set(count as i64);
}
pub fn inc_evicted_accounts_count() {
    EVICTED_ACCOUNTS_COUNT.inc();
}

pub fn observe_flush_accounts_time<T, F>(f: F) -> T
where
    F: FnOnce() -> T,
{
    FLUSH_ACCOUNTS_TIME_HISTOGRAM.observe_closure_duration(f)
}
