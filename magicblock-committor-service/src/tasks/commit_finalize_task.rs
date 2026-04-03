use dlp_api::{
    args::CommitFinalizeArgs,
    diff::compute_diff,
    instruction_builder::{commit_diff_size_budget, commit_size_budget},
    AccountSizeClass,
};
use magicblock_committor_program::Chunks;
use magicblock_core::intent::CommittedAccount;
use solana_account::{Account, ReadableAccount};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

use crate::{
    consts::MAX_WRITE_CHUNK_SIZE,
    tasks::{
        commit_task::{CommitBufferStage, CommitDelivery},
        BaseTask, BaseTaskImpl, PreparationTask,
    },
};

/// A task that commits a delegated account's state to the base layer and finalizes it in the same
/// instruction.
///
/// The delivery strategy ([`CommitDelivery`]) determines how the data reaches
/// the chain (inline args vs buffer, full state vs diff).
#[derive(Clone, Debug)]
pub struct CommitFinalizeTask {
    pub commit_id: u64,
    pub allow_undelegation: bool,
    pub committed_account: CommittedAccount,
    pub delivery: CommitDelivery,
}

impl CommitFinalizeTask {
    #[inline(always)]
    fn commit_finalize_ix(
        &self,
        validator: &Pubkey,
        base_account: Option<&Account>,
    ) -> Instruction {
        let (data, data_is_diff) = if let Some(base_account) = base_account {
            (
                compute_diff(
                    base_account.data(),
                    self.committed_account.account.data(),
                )
                .to_vec(),
                true,
            )
        } else {
            (self.committed_account.account.data.clone(), false)
        };

        let mut args = CommitFinalizeArgs {
            commit_id: self.commit_id,
            lamports: self.committed_account.account.lamports,
            data_is_diff: data_is_diff.into(),
            allow_undelegation: self.allow_undelegation.into(),
            bumps: Default::default(),
            reserved_padding: Default::default(),
        };

        dlp_api::instruction_builder::commit_finalize(
            *validator,
            self.committed_account.pubkey,
            &mut args,
            &data,
        )
        .0
    }

    #[inline(always)]
    fn commit_finalize_from_buffer_ix(
        &self,
        validator: &Pubkey,
        base_account: Option<&Account>,
    ) -> Instruction {
        let (data_buffer_pubkey, _) =
            magicblock_committor_program::pdas::buffer_pda(
                validator,
                &self.committed_account.pubkey,
                &self.commit_id.to_le_bytes(),
            );

        let mut args = CommitFinalizeArgs {
            commit_id: self.commit_id,
            lamports: self.committed_account.account.lamports,
            data_is_diff: base_account.is_some().into(),
            allow_undelegation: self.allow_undelegation.into(),
            bumps: Default::default(),
            reserved_padding: Default::default(),
        };

        dlp_api::instruction_builder::commit_finalize_from_buffer(
            *validator,
            self.committed_account.pubkey,
            data_buffer_pubkey,
            &mut args,
        )
        .0
    }

    pub fn stage(&self) -> Option<&CommitBufferStage> {
        match &self.delivery {
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
        match &mut self.delivery {
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
            self.delivery,
            CommitDelivery::StateInBuffer { .. }
                | CommitDelivery::DiffInBuffer { .. }
        )
    }

    pub fn state_preparation_stage(&self) -> CommitBufferStage {
        let data = self.committed_account.account.data.clone();
        self.preparation_stage(data)
    }

    fn diff_preparation_stage(&self, base_data: &[u8]) -> CommitBufferStage {
        let diff =
            compute_diff(base_data, &self.committed_account.account.data)
                .to_vec();
        self.preparation_stage(diff)
    }

    fn preparation_stage(&self, buffer_data: Vec<u8>) -> CommitBufferStage {
        let chunks =
            Chunks::from_data_length(buffer_data.len(), MAX_WRITE_CHUNK_SIZE);
        CommitBufferStage::Preparation(PreparationTask {
            commit_id: self.commit_id,
            pubkey: self.committed_account.pubkey,
            buffer_data,
            chunks,
        })
    }

    pub fn reset_commit_id(&mut self, commit_id: u64) {
        self.commit_id = commit_id;
        let new_stage = match &self.delivery {
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

        match &mut self.delivery {
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

impl BaseTask for CommitFinalizeTask {
    fn program_id(&self) -> Pubkey {
        dlp_api::id()
    }

    fn instruction(&self, validator: &Pubkey) -> Instruction {
        match &self.delivery {
            CommitDelivery::StateInArgs => {
                self.commit_finalize_ix(validator, None)
            }
            CommitDelivery::StateInBuffer { .. } => {
                self.commit_finalize_from_buffer_ix(validator, None)
            }
            CommitDelivery::DiffInArgs { base_account } => {
                self.commit_finalize_ix(validator, Some(base_account))
            }
            CommitDelivery::DiffInBuffer { base_account, .. } => self
                .commit_finalize_from_buffer_ix(validator, Some(base_account)),
        }
    }

    fn try_optimize_tx_size(&mut self) -> bool {
        let delivery =
            std::mem::replace(&mut self.delivery, CommitDelivery::StateInArgs);
        match delivery {
            CommitDelivery::StateInArgs => {
                let stage = self.state_preparation_stage();
                self.delivery = CommitDelivery::StateInBuffer { stage };
                true
            }
            CommitDelivery::DiffInArgs { base_account } => {
                let stage = self.diff_preparation_stage(base_account.data());
                self.delivery = CommitDelivery::DiffInBuffer {
                    base_account,
                    stage,
                };
                true
            }
            other @ (CommitDelivery::StateInBuffer { .. }
            | CommitDelivery::DiffInBuffer { .. }) => {
                self.delivery = other;
                false
            }
        }
    }

    fn compute_units(&self) -> u32 {
        70_000
    }

    fn accounts_size_budget(&self) -> u32 {
        match &self.delivery {
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

impl From<CommitFinalizeTask> for BaseTaskImpl {
    fn from(value: CommitFinalizeTask) -> Self {
        Self::CommitFinalize(value)
    }
}
