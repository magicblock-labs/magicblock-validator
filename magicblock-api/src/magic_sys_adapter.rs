use std::{collections::HashMap, sync::Arc, time::Duration};

use magicblock_committor_service::{
    committor_processor::CommittorProcessor,
    intent_executor::task_info_fetcher::TaskInfoFetcherResult,
    tasks::intent_size_validator::IntentSizeValidator,
};
use magicblock_core::{
    intent::{schedule::MagicIntentBundle, CommittedAccount},
    traits::MagicSys,
};
use magicblock_metrics::metrics;
use magicblock_program::magic_sys::INTENT_TOO_LARGE_ERR;
use solana_instruction::error::InstructionError;
use solana_pubkey::Pubkey;
use tracing::error;

#[derive(Clone)]
pub struct MagicSysAdapter {
    handle: tokio::runtime::Handle,
    committor_processor: Arc<CommittorProcessor>,
}

impl MagicSysAdapter {
    /// Returned when the sync channel is disconnected (sender dropped).
    const RECV_ERR: u32 = 0xE000_0000;
    /// Returned when waiting for the nonce fetch times out.
    const TIMEOUT_ERR: u32 = 0xE000_0001;
    /// Returned when the fetch of current commit nonces fails.
    const FETCH_ERR: u32 = 0xE000_0002;

    const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

    pub fn new(
        handle: tokio::runtime::Handle,
        committor_processor: Arc<CommittorProcessor>,
    ) -> Self {
        Self {
            handle,
            committor_processor,
        }
    }

    fn fetch_current_commit_nonces_sync(
        &self,
        pubkeys: &[Pubkey],
        min_context_slot: u64,
    ) -> std::sync::mpsc::Receiver<TaskInfoFetcherResult<HashMap<Pubkey, u64>>>
    {
        let (sender, receiver) = std::sync::mpsc::channel();
        let committor_processor = self.committor_processor.clone();
        let pubkeys = pubkeys.to_owned();

        // This is required to switch from TransactionExecutor runtime
        // blocking on it would cause a panic
        self.handle.spawn(async move {
            let result = committor_processor
                .fetch_current_commit_nonces(&pubkeys, min_context_slot)
                .await;
            if let Err(err) = sender.send(result) {
                error!(error = ?err, "Failed to send result back");
            }
        });

        receiver
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

        let min_context_slot = commits
            .iter()
            .map(|account| account.remote_slot)
            .max()
            .unwrap_or(0);
        let pubkeys: Vec<_> =
            commits.iter().map(|account| account.pubkey).collect();

        let _timer = metrics::start_fetch_commit_nonces_wait_timer();
        let receiver =
            self.fetch_current_commit_nonces_sync(&pubkeys, min_context_slot);
        receiver
            .recv_timeout(Self::FETCH_TIMEOUT)
            .map_err(|err| match err {
                std::sync::mpsc::RecvTimeoutError::Timeout => {
                    error!("Timed out waiting for commit nonces from CommittorService");
                    InstructionError::Custom(Self::TIMEOUT_ERR)
                }
                std::sync::mpsc::RecvTimeoutError::Disconnected => {
                    error!("CommittorService channel disconnected while waiting for commit nonces");
                    InstructionError::Custom(Self::RECV_ERR)
                }
            })?
            .inspect_err(|err| {
                error!(error = ?err, "Failed to fetch current commit nonces")
            })
            .map_err(|_| InstructionError::Custom(Self::FETCH_ERR))
    }

    fn validate_intent_size(
        &self,
        intent: &MagicIntentBundle,
    ) -> Result<(), InstructionError> {
        if IntentSizeValidator::fits(intent) {
            Ok(())
        } else {
            Err(InstructionError::Custom(INTENT_TOO_LARGE_ERR))
        }
    }
}
