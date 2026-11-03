use std::{collections::HashMap, time::Duration};

use committor::{CommitIdProvider, is_intent_within_limits};
use mbv_core::{
    intent::{MagicIntentBundle, types::CommittedAccount},
    traits::MagicSys,
};
use metrics::metrics;
use program::magic_sys::INTENT_TOO_LARGE_ERR;
use solana_instruction::error::InstructionError;
use solana_pubkey::Pubkey;
use tracing::error;

#[derive(Clone)]
pub(crate) struct MagicSysAdapter {
    handle: tokio::runtime::Handle,
    commit_id_provider: CommitIdProvider,
}

impl MagicSysAdapter {
    /// Returned when the fetch task panics or is cancelled.
    const TASK_ERR: u32 = 0xE000_0000;
    /// Returned when waiting for the nonce fetch times out.
    const TIMEOUT_ERR: u32 = 0xE000_0001;
    /// Returned when the fetch of current commit nonces fails.
    const FETCH_ERR: u32 = 0xE000_0002;

    const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

    pub(crate) fn new(
        handle: tokio::runtime::Handle,
        commit_id_provider: CommitIdProvider,
    ) -> Self {
        Self { handle, commit_id_provider }
    }
}

impl MagicSys for MagicSysAdapter {
    fn fetch_current_commit_nonces(
        &self,
        commits: &[CommittedAccount],
    ) -> Result<HashMap<Pubkey, u64>, InstructionError> {
        if commits.is_empty() {
            return Ok(HashMap::new());
        }

        let min_context_slot = commits.iter().map(|account| account.remote_slot).max().unwrap_or(0);
        let accounts: Vec<_> =
            commits.iter().map(|account| (account.pubkey, account.remote_slot)).collect();

        let _timer = metrics::start_fetch_commit_nonces_wait_timer();
        let provider = self.commit_id_provider.clone();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        self.handle.spawn(async move {
            let result = provider.current_commit_ids(&accounts, min_context_slot).await;
            let _ = sender.send(result);
        });

        // Simulation can call this from a Tokio runtime thread. Wait on a
        // standard channel instead of trying to start that runtime again.
        receiver
            .recv_timeout(Self::FETCH_TIMEOUT)
            .map_err(|err| match err {
                std::sync::mpsc::RecvTimeoutError::Timeout => {
                    error!("Timed out waiting for commit nonces from CommittorService");
                    InstructionError::Custom(Self::TIMEOUT_ERR)
                }
                std::sync::mpsc::RecvTimeoutError::Disconnected => {
                    error!("Commit nonce fetch task failed");
                    InstructionError::Custom(Self::TASK_ERR)
                }
            })?
            .inspect_err(|err| error!(error = ?err, "Failed to fetch current commit nonces"))
            .map_err(|_| InstructionError::Custom(Self::FETCH_ERR))
    }

    fn validate_intent_size(&self, intent: &MagicIntentBundle) -> Result<(), InstructionError> {
        if is_intent_within_limits(intent) {
            Ok(())
        } else {
            Err(InstructionError::Custom(INTENT_TOO_LARGE_ERR))
        }
    }
}
