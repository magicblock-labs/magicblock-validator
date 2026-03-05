use std::{collections::HashMap, error::Error, sync::Arc};

use magicblock_committor_service::{BaseIntentCommittor, CommittorService};
use magicblock_core::{intent::CommittedAccount, traits::MagicSys};
use magicblock_ledger::Ledger;
use solana_instruction::error::InstructionError;
use solana_pubkey::Pubkey;
use tracing::{enabled, error, trace, Level};

#[derive(Clone)]
pub struct MagicSysAdapter {
    ledger: Arc<Ledger>,
    committor_service: Option<Arc<CommittorService>>,
}

impl MagicSysAdapter {
    /// Returned when receiving the nonce result from the async channel fails.
    const RECV_ERR: u32 = 0xE000_0000;
    /// Returned when the async fetch of current commit nonces fails.
    const FETCH_ERR: u32 = 0xE001_0000;
    /// Returned when no committor service is configured.
    const NO_COMMITTOR_ERR: u32 = 0xE002_0000;

    pub fn new(
        ledger: Arc<Ledger>,
        committor_service: Option<Arc<CommittorService>>,
    ) -> Self {
        Self {
            ledger,
            committor_service,
        }
    }
}

impl MagicSys for MagicSysAdapter {
    fn persist(&self, id: u64, data: Vec<u8>) -> Result<(), Box<dyn Error>> {
        trace!(id, data_len = data.len(), "Persisting data");
        self.ledger.write_account_mod_data(id, &data.into())?;
        Ok(())
    }

    fn load(&self, id: u64) -> Result<Option<Vec<u8>>, Box<dyn Error>> {
        let data = self.ledger.read_account_mod_data(id)?.map(|x| x.data);
        if enabled!(Level::TRACE) {
            if let Some(data) = &data {
                trace!(id, data_len = data.len(), "Loading data");
            } else {
                trace!(id, found = false, "Loading data");
            }
        }
        Ok(data)
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

        let receiver = committor_service
            .fetch_current_commit_nonces(&pubkeys, min_context_slot);
        // Tx execution is sync and runs on a tokio worker thread. handle.block_on
        // would panic (nested runtime). futures::executor::block_on parks this
        // thread independently of tokio — safe because the thread is already
        // committed to this tx until execution completes.
        futures::executor::block_on(receiver)
            .inspect_err(|err| {
                error!(error = ?err, "Failed to receive nonces from CommittorService")
            })
            .map_err(|_| InstructionError::Custom(Self::RECV_ERR))?
            .inspect_err(|err| {
                error!(error = ?err, "Failed to fetch current commit nonces")
            })
            .map_err(|_| InstructionError::Custom(Self::FETCH_ERR))
    }
}
