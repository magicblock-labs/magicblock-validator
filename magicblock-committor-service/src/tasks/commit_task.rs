use dlp::{
    args::{CommitDiffArgs, CommitStateArgs, CommitStateFromBufferArgs},
    compute_diff,
    instruction_builder::{commit_diff_size_budget, commit_size_budget},
    AccountSizeClass,
};
use magicblock_committor_program::Chunks;
use magicblock_program::magic_scheduled_base_intent::CommittedAccount;
use solana_account::{Account, ReadableAccount};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

use crate::{
    consts::MAX_WRITE_CHUNK_SIZE,
    tasks::{BaseTask, BaseTaskImpl, CleanupTask, PreparationTask},
};

/// Lifecycle stage of a buffer used for commit delivery.
/// Tracks whether the on-chain buffer still needs to be initialized
/// or is ready to be cleaned up after a successful commit.
#[derive(Clone, Debug)]
pub enum CommitBufferStage {
    Preparation(PreparationTask),
    Cleanup(CleanupTask),
}

/// Describes how commit data is delivered to the base layer.
///
/// Small accounts send data directly in instruction args.
/// Large accounts use an on-chain buffer to avoid transaction size limits.
/// When a base account is available, a diff is computed to reduce payload size.
#[derive(Clone, Debug)]
pub enum CommitDelivery {
    StateInArgs,
    StateInBuffer {
        stage: CommitBufferStage,
    },
    DiffInArgs {
        base_account: Account,
    },
    DiffInBuffer {
        base_account: Account,
        stage: CommitBufferStage,
    },
}

/// A task that commits a delegated account's state to the base layer.
///
/// The delivery strategy ([`CommitDelivery`]) determines how the data reaches
/// the chain (inline args vs buffer, full state vs diff).
#[derive(Clone, Debug)]
pub struct CommitTask {
    pub commit_id: u64,
    pub allow_undelegation: bool,
    pub committed_account: CommittedAccount,
    pub delivery_details: CommitDelivery,
}

impl CommitTask {
    #[inline(always)]
    fn commit_state_ix(&self, validator: &Pubkey) -> Instruction {
        let args = CommitStateArgs {
            nonce: self.commit_id,
            lamports: self.committed_account.account.lamports,
            data: self.committed_account.account.data.clone(),
            allow_undelegation: self.allow_undelegation,
        };
        dlp::instruction_builder::commit_state(
            *validator,
            self.committed_account.pubkey,
            self.committed_account.account.owner,
            args,
        )
    }

    #[inline(always)]
    fn commit_state_from_buffer_ix(&self, validator: &Pubkey) -> Instruction {
        let (commit_buffer_pubkey, _) =
            magicblock_committor_program::pdas::buffer_pda(
                validator,
                &self.committed_account.pubkey,
                &self.commit_id.to_le_bytes(),
            );
        dlp::instruction_builder::commit_state_from_buffer(
            *validator,
            self.committed_account.pubkey,
            self.committed_account.account.owner,
            commit_buffer_pubkey,
            CommitStateFromBufferArgs {
                nonce: self.commit_id,
                lamports: self.committed_account.account.lamports,
                allow_undelegation: self.allow_undelegation,
            },
        )
    }

    #[inline(always)]
    fn commit_diff_ix(
        &self,
        validator: &Pubkey,
        base_account: &Account,
    ) -> Instruction {
        let args = CommitDiffArgs {
            nonce: self.commit_id,
            lamports: self.committed_account.account.lamports,
            diff: compute_diff(
                base_account.data(),
                self.committed_account.account.data(),
            )
            .to_vec(),
            allow_undelegation: self.allow_undelegation,
        };
        dlp::instruction_builder::commit_diff(
            *validator,
            self.committed_account.pubkey,
            self.committed_account.account.owner,
            args,
        )
    }

    #[inline(always)]
    fn commit_diff_from_buffer_ix(&self, validator: &Pubkey) -> Instruction {
        let (commit_buffer_pubkey, _) =
            magicblock_committor_program::pdas::buffer_pda(
                validator,
                &self.committed_account.pubkey,
                &self.commit_id.to_le_bytes(),
            );
        dlp::instruction_builder::commit_diff_from_buffer(
            *validator,
            self.committed_account.pubkey,
            self.committed_account.account.owner,
            commit_buffer_pubkey,
            CommitStateFromBufferArgs {
                nonce: self.commit_id,
                lamports: self.committed_account.account.lamports,
                allow_undelegation: self.allow_undelegation,
            },
        )
    }

    pub fn stage(&self) -> Option<&CommitBufferStage> {
        match &self.delivery_details {
            CommitDelivery::DiffInBuffer {
                base_account: _,
                stage,
            }
            | CommitDelivery::StateInBuffer { stage } => Some(stage),
            CommitDelivery::StateInArgs | CommitDelivery::DiffInArgs { .. } => {
                None
            }
        }
    }

    pub fn stage_mut(&mut self) -> Option<&mut CommitBufferStage> {
        match &mut self.delivery_details {
            CommitDelivery::DiffInBuffer {
                base_account: _,
                stage,
            }
            | CommitDelivery::StateInBuffer { stage } => Some(stage),
            CommitDelivery::StateInArgs | CommitDelivery::DiffInArgs { .. } => {
                None
            }
        }
    }

    pub fn is_buffer(&self) -> bool {
        matches!(
            self.delivery_details,
            CommitDelivery::StateInBuffer { .. }
                | CommitDelivery::DiffInBuffer { .. }
        )
    }

    pub fn state_preparation_stage(&self) -> CommitBufferStage {
        let committed_data = self.committed_account.account.data.clone();
        self.preparation_stage(committed_data)
    }

    fn diff_preparation_stage(&self, base_data: &[u8]) -> CommitBufferStage {
        let diff =
            compute_diff(base_data, &self.committed_account.account.data)
                .to_vec();
        self.preparation_stage(diff)
    }

    fn preparation_stage(&self, committed_data: Vec<u8>) -> CommitBufferStage {
        let chunks = Chunks::from_data_length(
            committed_data.len(),
            MAX_WRITE_CHUNK_SIZE,
        );
        CommitBufferStage::Preparation(PreparationTask {
            commit_id: self.commit_id,
            pubkey: self.committed_account.pubkey,
            committed_data,
            chunks,
        })
    }

    pub fn reset_commit_id(&mut self, commit_id: u64) {
        self.commit_id = commit_id;
        let new_stage = match &self.delivery_details {
            CommitDelivery::StateInBuffer { .. } => {
                self.state_preparation_stage()
            }
            CommitDelivery::DiffInBuffer {
                base_account,
                stage: _,
            } => {
                let slice = base_account.data.as_slice();
                self.diff_preparation_stage(slice)
            }
            _ => return,
        };

        match &mut self.delivery_details {
            CommitDelivery::StateInBuffer { stage } => {
                *stage = new_stage;
            }
            CommitDelivery::DiffInBuffer {
                base_account: _,
                stage,
            } => {
                *stage = new_stage;
            }
            _ => {}
        }
    }
}

impl BaseTask for CommitTask {
    fn instruction(&self, validator: &Pubkey) -> Instruction {
        match &self.delivery_details {
            CommitDelivery::StateInArgs => self.commit_state_ix(validator),
            CommitDelivery::StateInBuffer { .. } => {
                self.commit_state_from_buffer_ix(validator)
            }
            CommitDelivery::DiffInArgs { base_account } => {
                self.commit_diff_ix(validator, base_account)
            }
            CommitDelivery::DiffInBuffer { .. } => {
                self.commit_diff_from_buffer_ix(validator)
            }
        }
    }

    fn try_optimize_tx_size(&mut self) -> bool {
        let details = std::mem::replace(
            &mut self.delivery_details,
            CommitDelivery::StateInArgs,
        );
        match details {
            CommitDelivery::StateInArgs => {
                let stage = self.state_preparation_stage();
                self.delivery_details = CommitDelivery::StateInBuffer { stage };
                true
            }
            CommitDelivery::DiffInArgs { base_account } => {
                let stage = self.diff_preparation_stage(base_account.data());
                self.delivery_details = CommitDelivery::DiffInBuffer {
                    base_account,
                    stage,
                };
                true
            }
            other @ (CommitDelivery::StateInBuffer { .. }
            | CommitDelivery::DiffInBuffer { .. }) => {
                self.delivery_details = other;
                false
            }
        }
    }

    fn compute_units(&self) -> u32 {
        70_000
    }

    fn accounts_size_budget(&self) -> u32 {
        match &self.delivery_details {
            CommitDelivery::StateInArgs => {
                commit_size_budget(AccountSizeClass::Dynamic(
                    self.committed_account.account.data.len() as u32,
                ))
            }
            CommitDelivery::StateInBuffer { .. }
            | CommitDelivery::DiffInBuffer { .. } => {
                commit_size_budget(AccountSizeClass::Huge)
            }
            CommitDelivery::DiffInArgs { .. } => {
                commit_diff_size_budget(AccountSizeClass::Dynamic(
                    self.committed_account.account.data.len() as u32,
                ))
            }
        }
    }
}

impl From<CommitTask> for BaseTaskImpl {
    fn from(value: CommitTask) -> Self {
        Self::Commit(value)
    }
}
