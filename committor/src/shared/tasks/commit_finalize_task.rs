use delegation_program_interface::{
    self as dpi,
    diff::compute_diff,
    legacy::{
        AccountSizeClass,
        args::CommitFinalizeArgs,
        instruction_builder::{
            commit_finalize_from_buffer_size_budget, commit_finalize_size_budget,
        },
    },
};
use mbv_core::intent::types::CommittedAccount;
use solana_account::{Account, ReadableAccount};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use wheels::DataLayoutError;

use crate::shared::{
    account_state::DelegationProtocol,
    tasks::{Task, commit_delivery::CommitDelivery},
};

/// A task that commits a delegated account's state to the base layer and finalizes it in the same
/// instruction.
///
/// The delivery strategy ([`CommitDelivery`]) determines how the data reaches
/// the chain (inline args vs buffer, full state vs diff).
#[derive(Clone, Debug)]
pub struct CommitFinalizeTask {
    pub(crate) protocol: DelegationProtocol,
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
    ) -> Result<Instruction, DataLayoutError> {
        let (data, data_is_diff) = if let Some(base_account) = base_account {
            (
                compute_diff(base_account.data(), self.committed_account.account.data()).to_vec(),
                true,
            )
        } else {
            (self.committed_account.account.data.clone(), false)
        };

        if let DelegationProtocol::V2 { cluster } = self.protocol {
            return dpi::instruction_builder::commit_finalize(
                *validator,
                cluster,
                self.committed_account.pubkey,
                dpi::CommitFinalizeArgs {
                    commit_id: self.commit_id,
                    lamports: self.committed_account.account.lamports,
                    owner: self.committed_account.account.owner,
                    allow_undelegation: self.allow_undelegation,
                    payload_kind: if data_is_diff {
                        dpi::CommitPayloadKind::Diff
                    } else {
                        dpi::CommitPayloadKind::FullData
                    }
                    .value(),
                    payload_alignment_padding: 0,
                    payload: data,
                },
            );
        }
        let mut args = CommitFinalizeArgs {
            commit_id: self.commit_id,
            lamports: self.committed_account.account.lamports,
            data_is_diff: data_is_diff.into(),
            allow_undelegation: self.allow_undelegation.into(),
            bumps: Default::default(),
            reserved_padding: Default::default(),
        };

        Ok(dpi::legacy::instruction_builder::commit_finalize(
            *validator,
            self.committed_account.pubkey,
            &mut args,
            &data,
        )
        .0)
    }

    #[inline(always)]
    fn commit_finalize_from_buffer_ix(
        &self,
        validator: &Pubkey,
        base_account: Option<&Account>,
    ) -> Result<Instruction, DataLayoutError> {
        if let DelegationProtocol::V2 { cluster } = self.protocol {
            return dpi::instruction_builder::commit_finalize_from_buffer(
                *validator,
                cluster,
                self.committed_account.pubkey,
                dpi::CommitFinalizeFromBufferArgs {
                    commit_id: self.commit_id,
                    lamports: self.committed_account.account.lamports,
                    owner: self.committed_account.account.owner,
                    allow_undelegation: self.allow_undelegation,
                    payload_kind: if base_account.is_some() {
                        dpi::CommitPayloadKind::Diff
                    } else {
                        dpi::CommitPayloadKind::FullData
                    }
                    .value(),
                },
            );
        }
        let (data_buffer_pubkey, _) = committor_interface::pdas::buffer_pda(
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

        Ok(
            dpi::legacy::instruction_builder::commit_finalize_from_buffer(
                *validator,
                self.committed_account.pubkey,
                data_buffer_pubkey,
                &mut args,
            )
            .0,
        )
    }

    pub fn is_buffer(&self) -> bool {
        matches!(
            self.delivery,
            CommitDelivery::StateInBuffer | CommitDelivery::DiffInBuffer { .. }
        )
    }

    pub fn instruction(&self, validator: &Pubkey) -> Result<Instruction, DataLayoutError> {
        match &self.delivery {
            CommitDelivery::StateInArgs => self.commit_finalize_ix(validator, None),
            CommitDelivery::StateInBuffer => self.commit_finalize_from_buffer_ix(validator, None),
            CommitDelivery::DiffInArgs { base_account } => {
                self.commit_finalize_ix(validator, Some(base_account))
            }
            CommitDelivery::DiffInBuffer { base_account } => {
                self.commit_finalize_from_buffer_ix(validator, Some(base_account))
            }
        }
    }

    pub fn try_use_buffer(&mut self) -> bool {
        // CHECKPOINT: enable v2 buffers once their preparation and cleanup use DLP's
        // StateBuffer instructions: https://github.com/magicblock-labs/delegation-program/issues/270.
        // Until then, keep v2 payloads inline so we do not leave buffer rent behind.
        if matches!(self.protocol, DelegationProtocol::V2 { .. }) {
            return false;
        }
        let delivery = std::mem::replace(&mut self.delivery, CommitDelivery::StateInArgs);
        match delivery {
            CommitDelivery::StateInArgs => {
                self.delivery = CommitDelivery::StateInBuffer;
                true
            }
            CommitDelivery::DiffInArgs { base_account } => {
                self.delivery = CommitDelivery::DiffInBuffer { base_account };
                true
            }
            other @ (CommitDelivery::StateInBuffer | CommitDelivery::DiffInBuffer { .. }) => {
                self.delivery = other;
                false
            }
        }
    }

    pub fn compute_units(&self) -> u32 {
        120_000
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
