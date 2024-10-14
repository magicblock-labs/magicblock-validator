use std::sync::Once;

use prometheus::{IntCounter, IntCounterVec, Opts, Registry};

lazy_static::lazy_static! {
    pub(crate) static ref REGISTRY: Registry = Registry::new();

    pub static ref SLOT_COUNT: IntCounter = IntCounter::new(
        "mbv_slot_count", "Slot Count",
    ).unwrap();

    pub static ref TRANSACTION_VEC_COUNT: IntCounterVec = IntCounterVec::new(
        Opts::new("mbv_transaction_count", "Transaction Count"),
        &["outcome"],
    ).unwrap();

    pub static ref EXECUTED_UNITS_COUNT: IntCounter = IntCounter::new(
        "mbv_executed_units_count", "Executed Units (CU) Count",
    ).unwrap();

    pub static ref FEE_COUNT: IntCounter = IntCounter::new(
        "mbv_fee_count", "Fee Count",
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
        register!(EXECUTED_UNITS_COUNT);
        register!(FEE_COUNT);
    });
}

pub fn inc_slot() {
    SLOT_COUNT.inc();
}

pub fn inc_transaction(is_ok: bool) {
    if is_ok {
        TRANSACTION_VEC_COUNT.with_label_values(&["success"]).inc();
    } else {
        TRANSACTION_VEC_COUNT.with_label_values(&["error"]).inc();
    }
}

pub fn inc_executed_units(executed_units: u64) {
    EXECUTED_UNITS_COUNT.inc_by(executed_units);
}

pub fn inc_fee(fee: u64) {
    FEE_COUNT.inc_by(fee);
}
