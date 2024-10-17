use std::sync::Once;

use prometheus::{
    Histogram, HistogramOpts, IntCounter, IntCounterVec, IntGauge, Opts,
    Registry,
};
pub use types::{AccountClone, AccountCommit};
mod types;

lazy_static::lazy_static! {
    pub(crate) static ref REGISTRY: Registry = Registry::new_custom(Some("mbv".to_string()), None).unwrap();

    pub static ref SLOT_COUNT: IntCounter = IntCounter::new(
        "slot_count", "Slot Count",
    ).unwrap();

    pub static ref TRANSACTION_VEC_COUNT: IntCounterVec = IntCounterVec::new(
        Opts::new("transaction_count", "Transaction Count"),
        &["outcome"],
    ).unwrap();

    pub static ref FEE_PAYER_VEC_COUNT: IntCounterVec = IntCounterVec::new(
        Opts::new("fee_payer_count", "Count of transactions signed by specific fee payers"),
        &["fee_payer", "outcome"],
    ).unwrap();

    pub static ref EXECUTED_UNITS_COUNT: IntCounter = IntCounter::new(
        "executed_units_count", "Executed Units (CU) Count",
    ).unwrap();

    pub static ref FEE_COUNT: IntCounter = IntCounter::new(
        "fee_count", "Fee Count",
    ).unwrap();

    pub static ref ACCOUNT_CLONE_VEC_COUNT: IntCounterVec = IntCounterVec::new(
        Opts::new("account_clone_count", "Count clones performed for specific accounts"),
        &["kind", "pubkey", "owner"],
    ).unwrap();

    pub static ref ACCOUNT_COMMIT_VEC_COUNT: IntCounterVec = IntCounterVec::new(
        Opts::new("account_commit_count", "Count commits performed for specific accounts"),
        &["kind", "pubkey"],
    ).unwrap();

    pub static ref LEDGER_SIZE_GAUGE: IntGauge = IntGauge::new(
        "ledger_size", "Ledger Size in Bytes",
    ).unwrap();

    pub static ref SIGVERIFY_TIME_HISTOGRAM: Histogram = Histogram::with_opts(
        HistogramOpts::new("sigverify_time", "Time spent in sigverify")
            .buckets(vec![
                // 10µs - 90µs
                0.000_01, 0.000_02, 0.000_03, 0.000_04, 0.000_05,
                0.000_06, 0.000_07, 0.000_08, 0.000_09,
                // 100µs - 900µs
                0.000_1, 0.000_2, 0.000_3, 0.000_4, 0.000_5,
                0.000_6, 0.000_7, 0.000_8, 0.000_9,
                // 1ms - 9ms
                0.001, 0.002, 0.003, 0.004, 0.005,
                0.006, 0.007, 0.008, 0.009,
            ]),
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
        register!(LEDGER_SIZE_GAUGE);
        register!(SIGVERIFY_TIME_HISTOGRAM);
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
        CommitOnly { pubkey } => {
            ACCOUNT_COMMIT_VEC_COUNT
                .with_label_values(&["commit", pubkey])
                .inc();
        }
        CommitAndUndelegate { pubkey } => {
            ACCOUNT_COMMIT_VEC_COUNT
                .with_label_values(&["commit_and_undelegate", pubkey])
                .inc();
        }
    }
}

pub fn set_ledger_size(size: u64) {
    LEDGER_SIZE_GAUGE.set(size as i64);
}

pub fn observe_sigverify_time<T, F>(f: F) -> T
where
    F: FnOnce() -> T,
{
    SIGVERIFY_TIME_HISTOGRAM.observe_closure_duration(f)
}
