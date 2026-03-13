use std::{collections::HashMap, error::Error, sync::Arc, time::Duration};

use magicblock_committor_service::CommittorService;
use magicblock_core::{intent::CommittedAccount, traits::MagicSys};
use magicblock_metrics::metrics;
use solana_instruction::error::InstructionError;
use solana_pubkey::Pubkey;
use tracing::{error, trace};

#[derive(Clone)]
pub struct MagicSysAdapter {
    committor_service: Option<Arc<CommittorService>>,
}

impl MagicSysAdapter {
    /// Returned when the sync channel is disconnected (sender dropped).
    const RECV_ERR: u32 = 0xE000_0000;
    /// Returned when waiting for the nonce fetch times out.
    const TIMEOUT_ERR: u32 = 0xE000_0001;
    /// Returned when the fetch of current commit nonces fails.
    const FETCH_ERR: u32 = 0xE000_0002;
    /// Returned when no committor service is configured.
    const NO_COMMITTOR_ERR: u32 = 0xE000_0003;

    const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

    pub fn new(committor_service: Option<Arc<CommittorService>>) -> Self {
        Self { committor_service }
    }
}

impl MagicSys for MagicSysAdapter {
    fn persist(&self, id: u64, data: Vec<u8>) -> Result<(), Box<dyn Error>> {
        trace!(id, data_len = data.len(), "Persisting data");
        Ok(())
    }

    fn load(&self, _id: u64) -> Result<Option<Vec<u8>>, Box<dyn Error>> {
        Ok(None)
    }

    fn fetch_current_commit_nonces(
        &self,
        commits: &[CommittedAccount],
    ) -> Result<HashMap<Pubkey, u64>, InstructionError> {
        if commits.is_empty() {
            return Ok(HashMap::new());
        }
        let committor_service =
            if let Some(committor_service) = &self.committor_service {
                Ok(committor_service)
            } else {
                Err(InstructionError::Custom(Self::NO_COMMITTOR_ERR))
            }?;

        let min_context_slot = commits
            .iter()
            .map(|account| account.remote_slot)
            .max()
            .unwrap_or(0);
        let pubkeys: Vec<_> =
            commits.iter().map(|account| account.pubkey).collect();

        let _timer = metrics::start_fetch_commit_nonces_wait_timer();
        let receiver = committor_service
            .fetch_current_commit_nonces_sync(&pubkeys, min_context_slot);
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
}
