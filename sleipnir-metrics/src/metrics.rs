use prometheus::{IntCounter, Registry};
use std::sync::Once;

lazy_static::lazy_static! {
    pub static ref REGISTRY: Registry = Registry::new();

    pub static ref SLOT: IntCounter = IntCounter::new(
        "mbv_current_slot", "Current slot",
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
        register!(SLOT);
    });
}
