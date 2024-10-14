use std::sync::Once;

use prometheus::{IntCounter, Registry};

lazy_static::lazy_static! {
    pub(crate) static ref REGISTRY: Registry = Registry::new();

    pub static ref SLOT_COUNTER: IntCounter = IntCounter::new(
        "mbv_slot_counter", "Slot Counter",
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
        register!(SLOT_COUNTER);
    });
}

pub fn inc_slot() {
    SLOT_COUNTER.inc();
}
