use std::collections::HashSet;

use solana_pubkey::Pubkey;

use crate::shared::tasks::buffer_task::BufferCleanup;

/// Resources acquired by one preparation attempt, retained across cancellation.
/// Separate attempts may reserve the same ALT keys; each reservation must be released.
#[derive(Debug, Default)]
#[cfg_attr(test, derive(Clone))]
pub(crate) struct CleanupResources {
    pub(crate) buffers: Vec<BufferCleanup>,
    pub(crate) alt_reservation: HashSet<Pubkey>,
    pub(crate) uniqueness_nonce: Option<u64>,
}
