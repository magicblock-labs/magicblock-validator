use std::collections::HashSet;

use solana_pubkey::Pubkey;

use crate::tasks::buffer_task::CleanupTask;

/// Resources acquired by one preparation attempt, retained across cancellation.
/// Separate attempts may reserve the same ALT keys; each reservation must be released.
#[derive(Debug, Default)]
#[cfg_attr(test, derive(Clone))]
pub struct CleanupResources {
    pub(crate) buffers: Vec<CleanupTask>,
    pub(crate) alt_reservation: HashSet<Pubkey>,
    pub(crate) uniqueness_nonce: Option<u64>,
}
