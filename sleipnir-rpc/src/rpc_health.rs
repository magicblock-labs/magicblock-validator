// NOTE: from rpc/src/rpc_health.rs
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum RpcHealthStatus {
    Ok,
    Unknown,
}

pub struct RpcHealth {
    startup_verification_complete: Arc<AtomicBool>,
}

impl RpcHealth {
    pub(crate) fn new(startup_verification_complete: Arc<AtomicBool>) -> Self {
        Self {
            startup_verification_complete,
        }
    }

    pub(crate) fn check(&self) -> RpcHealthStatus {
        if !self.startup_verification_complete.load(Ordering::Acquire) {
            RpcHealthStatus::Unknown
        } else {
            RpcHealthStatus::Ok
        }
    }
}
