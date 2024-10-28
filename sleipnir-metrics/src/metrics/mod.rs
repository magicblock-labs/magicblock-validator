use std::sync::Once;

pub use prometheus::HistogramTimer;
use prometheus::{
    Histogram, HistogramOpts, IntCounter, IntCounterVec, IntGauge, Opts,
    Registry,
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

    static ref LEDGER_SIZE_GAUGE: IntGauge = IntGauge::new(
        "ledger_size", "Ledger Size in Bytes",
    ).unwrap();

    static ref ACCOUNTS_SIZE_GAUGE: IntGauge = IntGauge::new(
        "accounts_size", "Size of persisted accounts currently on disk",
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
        register!(LEDGER_SIZE_GAUGE);
        register!(ACCOUNTS_SIZE_GAUGE);
        register!(SIGVERIFY_TIME_HISTOGRAM);
        register!(ENSURE_ACCOUNTS_TIME_HISTOGRAM);
        register!(TRANSACTION_EXECUTION_TIME_HISTORY);
        register!(FLUSH_ACCOUNTS_TIME_HISTOGRAM);
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
        Wallet { pubkey } => {
            ACCOUNT_CLONE_VEC_COUNT
                .with_label_values(&["wallet", pubkey, ""])
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

pub fn account_commit_end(timer: HistogramTimer) {
    timer.stop_and_record();
}

pub fn set_ledger_size(size: u64) {
    LEDGER_SIZE_GAUGE.set(size as i64);
}

pub fn set_accounts_size(size: u64) {
    ACCOUNTS_SIZE_GAUGE.set(size as i64);
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

pub fn observe_flush_accounts_time<T, F>(f: F) -> T
where
    F: FnOnce() -> T,
{
    FLUSH_ACCOUNTS_TIME_HISTOGRAM.observe_closure_duration(f)
}
