use delegation_program_api::{
    AccountSizeClass,
    args::CommitFinalizeArgs,
    instruction_builder::{commit_finalize_from_buffer_size_budget, commit_finalize_size_budget},
};
use mbv_core::intent::types::CommittedAccount;
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

use crate::shared::tasks::{Task, commit_delivery::CommitDelivery};

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
    fn commit_finalize_ix(&self, validator: &Pubkey, diff: Option<&[u8]>) -> Instruction {
        let mut args = CommitFinalizeArgs {
            commit_id: self.commit_id,
            lamports: self.committed_account.account.lamports,
            data_is_diff: diff.is_some().into(),
            allow_undelegation: self.allow_undelegation.into(),
            bumps: Default::default(),
            reserved_padding: Default::default(),
        };

        delegation_program_api::instruction_builder::commit_finalize(
            *validator,
            self.committed_account.pubkey,
            &mut args,
            diff.unwrap_or(&self.committed_account.account.data),
        )
        .0
    }

    #[inline(always)]
    fn commit_finalize_from_buffer_ix(
        &self,
        validator: &Pubkey,
        data_is_diff: bool,
    ) -> Instruction {
        let (data_buffer_pubkey, _) = committor_interface::pdas::buffer_pda(
            validator,
            &self.committed_account.pubkey,
            &self.commit_id.to_le_bytes(),
        );

        let mut args = CommitFinalizeArgs {
            commit_id: self.commit_id,
            lamports: self.committed_account.account.lamports,
            data_is_diff: data_is_diff.into(),
            allow_undelegation: self.allow_undelegation.into(),
            bumps: Default::default(),
            reserved_padding: Default::default(),
        };

        delegation_program_api::instruction_builder::commit_finalize_from_buffer(
            *validator,
            self.committed_account.pubkey,
            data_buffer_pubkey,
            &mut args,
        )
        .0
    }

    pub fn is_buffer(&self) -> bool {
        matches!(
            self.delivery,
            CommitDelivery::StateInBuffer | CommitDelivery::DiffInBuffer { .. }
        )
    }

    pub fn instruction(&self, validator: &Pubkey) -> Instruction {
        match &self.delivery {
            CommitDelivery::StateInArgs => self.commit_finalize_ix(validator, None),
            CommitDelivery::StateInBuffer => self.commit_finalize_from_buffer_ix(validator, false),
            CommitDelivery::DiffInArgs { diff } => self.commit_finalize_ix(validator, Some(diff)),
            CommitDelivery::DiffInBuffer { .. } => {
                self.commit_finalize_from_buffer_ix(validator, true)
            }
        }
    }

    pub fn try_use_buffer(&mut self) -> bool {
        let delivery = std::mem::replace(&mut self.delivery, CommitDelivery::StateInArgs);
        match delivery {
            CommitDelivery::StateInArgs => {
                self.delivery = CommitDelivery::StateInBuffer;
                true
            }
            CommitDelivery::DiffInArgs { diff } => {
                self.delivery = CommitDelivery::DiffInBuffer { diff };
                true
            }
            other @ (CommitDelivery::StateInBuffer | CommitDelivery::DiffInBuffer { .. }) => {
                self.delivery = other;
                false
            }
        }
    }

    pub fn compute_units(&self) -> u32 {
        match &self.delivery {
            // Buffered diffs have consumed over 185,000 CU per commit on devnet.
            // Inline diffs are bounded by the transaction size.
            CommitDelivery::DiffInBuffer { .. } => 300_000,
            _ => 120_000,
        }
    }

    pub fn accounts_size_budget(&self) -> u32 {
        match &self.delivery {
            CommitDelivery::StateInArgs | CommitDelivery::DiffInArgs { .. } => {
                commit_finalize_size_budget(AccountSizeClass::Dynamic(
                    self.committed_account.account.data.len() as u32,
                ))
            }
            CommitDelivery::StateInBuffer | CommitDelivery::DiffInBuffer { .. } => {
                commit_finalize_from_buffer_size_budget(AccountSizeClass::Huge)
            }
        }
    }
}

impl From<CommitFinalizeTask> for Task {
    fn from(value: CommitFinalizeTask) -> Self {
        Self::CommitFinalize(value)
    }
}
