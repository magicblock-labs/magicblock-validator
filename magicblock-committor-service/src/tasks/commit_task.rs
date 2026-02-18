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
    tasks::{BaseTaskImpl, CleanupTask, PreparationTask},
};

// TODO: rename
#[derive(Clone, Debug)]
pub enum CommitStage {
    Preparation(PreparationTask),
    Cleanup(CleanupTask),
}

// TODO: rename
#[derive(Clone, Debug)]
pub enum CommitDeliveryDetails {
    StateInArgs,
    StateInBuffer {
        stage: CommitStage,
    },
    DiffInArgs {
        base_account: Account,
    },
    DiffInBuffer {
        base_account: Account,
        stage: CommitStage,
    },
}

#[derive(Clone, Debug)]
pub struct CommitTaskV2 {
    pub commit_id: u64,
    pub allow_undelegation: bool,
    pub committed_account: CommittedAccount,
    // TODO(edwin): define visibility
    pub delivery_details: CommitDeliveryDetails,
}

impl CommitTaskV2 {
    pub fn instruction(&self, validator: &Pubkey) -> Instruction {
        match &self.delivery_details {
            // TODO(edwin): extract into separate functions, inline
            CommitDeliveryDetails::StateInArgs => {
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
            CommitDeliveryDetails::StateInBuffer { .. } => {
                let commit_id_slice = self.commit_id.to_le_bytes();
                let (commit_buffer_pubkey, _) =
                    magicblock_committor_program::pdas::buffer_pda(
                        validator,
                        &self.committed_account.pubkey,
                        &commit_id_slice,
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
            CommitDeliveryDetails::DiffInArgs { base_account } => {
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
            CommitDeliveryDetails::DiffInBuffer { .. } => {
                let commit_id_slice = self.commit_id.to_le_bytes();
                let (commit_buffer_pubkey, _) =
                    magicblock_committor_program::pdas::buffer_pda(
                        validator,
                        &self.committed_account.pubkey,
                        &commit_id_slice,
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
        }
    }

    pub fn stage(&self) -> Option<&CommitStage> {
        match &self.delivery_details {
            CommitDeliveryDetails::DiffInBuffer {
                base_account: _,
                stage,
            }
            | CommitDeliveryDetails::StateInBuffer { stage } => Some(stage),
            CommitDeliveryDetails::StateInArgs
            | CommitDeliveryDetails::DiffInArgs { .. } => None,
        }
    }

    pub fn stage_mut(&mut self) -> Option<&mut CommitStage> {
        match &mut self.delivery_details {
            CommitDeliveryDetails::DiffInBuffer {
                base_account: _,
                stage,
            }
            | CommitDeliveryDetails::StateInBuffer { stage } => Some(stage),
            CommitDeliveryDetails::StateInArgs
            | CommitDeliveryDetails::DiffInArgs { .. } => None,
        }
    }

    pub fn is_buffer(&self) -> bool {
        matches!(
            self.delivery_details,
            CommitDeliveryDetails::StateInBuffer { .. }
                | CommitDeliveryDetails::DiffInBuffer { .. }
        )
    }

    pub fn state_preparation_stage(&self) -> CommitStage {
        let committed_data = self.committed_account.account.data.clone();
        self.preparation_stage(committed_data)
    }

    fn diff_preparation_stage(&self, base_data: &[u8]) -> CommitStage {
        let diff =
            compute_diff(base_data, &self.committed_account.account.data)
                .to_vec();
        self.preparation_stage(diff)
    }

    fn preparation_stage(&self, committed_data: Vec<u8>) -> CommitStage {
        let chunks = Chunks::from_data_length(
            committed_data.len(),
            MAX_WRITE_CHUNK_SIZE,
        );
        CommitStage::Preparation(PreparationTask {
            commit_id: self.commit_id,
            pubkey: self.committed_account.pubkey,
            committed_data,
            chunks,
        })
    }

    pub fn try_optimize_tx_size(mut self) -> Result<Self, Self> {
        let stage = match &self.delivery_details {
            CommitDeliveryDetails::StateInArgs => {
                self.state_preparation_stage()
            }
            CommitDeliveryDetails::DiffInArgs { base_account } => {
                self.diff_preparation_stage(&base_account.data)
            }
            CommitDeliveryDetails::DiffInBuffer { .. }
            | CommitDeliveryDetails::StateInBuffer { .. } => return Err(self),
        };

        match self.delivery_details {
            CommitDeliveryDetails::StateInArgs => {
                self.delivery_details =
                    CommitDeliveryDetails::StateInBuffer { stage };
            }
            CommitDeliveryDetails::DiffInArgs { base_account } => {
                self.delivery_details = CommitDeliveryDetails::DiffInBuffer {
                    stage,
                    base_account,
                };
            }
            _ => {}
        }
        Ok(self)
    }

    pub fn compute_units(&self) -> u32 {
        70_000
    }

    pub fn accounts_size_budget(&self) -> u32 {
        match &self.delivery_details {
            CommitDeliveryDetails::StateInArgs => {
                commit_size_budget(AccountSizeClass::Dynamic(
                    self.committed_account.account.data.len() as u32,
                ))
            }
            CommitDeliveryDetails::StateInBuffer { .. }
            | CommitDeliveryDetails::DiffInBuffer { .. } => {
                commit_size_budget(AccountSizeClass::Huge)
            }
            CommitDeliveryDetails::DiffInArgs { .. } => {
                commit_diff_size_budget(AccountSizeClass::Dynamic(
                    self.committed_account.account.data.len() as u32,
                ))
            }
        }
    }

    pub fn reset_commit_id(&mut self, commit_id: u64) {
        self.commit_id = commit_id;
        let new_stage = match &self.delivery_details {
            CommitDeliveryDetails::StateInBuffer { .. } => {
                self.state_preparation_stage()
            }
            CommitDeliveryDetails::DiffInBuffer {
                base_account,
                stage: _,
            } => {
                let slice = base_account.data.as_slice();
                self.diff_preparation_stage(slice)
            }
            _ => return,
        };

        match &mut self.delivery_details {
            CommitDeliveryDetails::StateInBuffer { stage } => {
                *stage = new_stage;
            }
            CommitDeliveryDetails::DiffInBuffer {
                base_account: _,
                stage,
            } => {
                *stage = new_stage;
            }
            _ => {}
        }
    }
}

impl From<CommitTaskV2> for BaseTaskImpl {
    fn from(value: CommitTaskV2) -> Self {
        Self::Commit(value)
    }
}
