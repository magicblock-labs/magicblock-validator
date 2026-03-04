use std::{error::Error, sync::Arc};

use magicblock_committor_service::{BaseIntentCommittor, CommittorService};
use magicblock_core::{
    intent::CommittedAccount,
    traits::{MagicSys, NONCE_LIMIT_ERR},
};
use magicblock_ledger::Ledger;
use solana_instruction::error::InstructionError;
use solana_pubkey::Pubkey;
use tracing::{enabled, error, trace, Level};

const NONCE_LIMIT: u64 = 400;

#[derive(Clone)]
pub struct MagicSysAdapter {
    handle: tokio::runtime::Handle,
    ledger: Arc<Ledger>,
    committor_service: Arc<CommittorService>,
}

impl MagicSysAdapter {
    const RECV_ERR: u32 = 0xE000_0000;
    const FETCH_ERR: u32 = 0xE001_0000;

    pub fn new(
        ledger: Arc<Ledger>,
        committor_service: Arc<CommittorService>,
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

    fn validate_commits(
        &self,
        commits: &[CommittedAccount],
    ) -> Result<(), InstructionError> {
        let min_context_slot = commits
            .iter()
            .map(|account| account.remote_slot)
            .max()
            .unwrap_or(0);
        let pubkeys: Vec<_> =
            commits.iter().map(|account| account.pubkey).collect();

        let receiver = self
            .committor_service
            .fetch_current_commit_nonces(&pubkeys, min_context_slot);
        let nonces_map = self.handle.block_on(receiver)
            .inspect_err(|err| {
                error!(error = ?err, "Failed to receive nonces from CommittorService")
            })
            .map_err(|err| InstructionError::Custom(Self::RECV_ERR))?
            .inspect_err(|err| {
                error!(error = ?err, "Failed to fetch current commit nonces")
            })
            .map_err(|err| InstructionError::Custom(Self::FETCH_ERR))?;

        for (pubkey, nonce) in nonces_map {
            if nonce >= NONCE_LIMIT {
                trace!("Limit of commits exceeded for: {}", pubkey);
                return Err(InstructionError::Custom(NONCE_LIMIT_ERR));
            }
        }

        Ok(())
    }
}
