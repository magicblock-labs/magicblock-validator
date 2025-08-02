use std::sync::Arc;

use solana_signature::Signature;

use crate::Slot;

use super::ExpiringCache;

pub type TransactionsCache = Arc<ExpiringCache<Signature, SignatureStatus>>;

#[derive(Clone, Copy)]
pub(crate) struct SignatureStatus {
    pub slot: Slot,
    pub successful: bool,
}
