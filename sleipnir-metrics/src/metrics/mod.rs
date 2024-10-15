use std::sync::Once;

use prometheus::{IntCounter, IntCounterVec, Opts, Registry};
pub use types::AccountClone;
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
