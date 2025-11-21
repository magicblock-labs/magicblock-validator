use core::panic;
use std::sync::Arc;

use dlp::{
    args::{CommitDiffArgs, CommitStateArgs, CommitStateFromBufferArgs},
    compute_diff,
};
use magicblock_program::magic_scheduled_base_intent::CommittedAccount;
use solana_account::{Account, ReadableAccount};
use solana_pubkey::Pubkey;
use solana_sdk::instruction::Instruction;

use crate::intent_executor::task_info_fetcher::TaskInfoFetcher;

use super::{BufferLifecycle, TaskStrategy};

#[derive(Debug, Clone)]
pub enum CommitStrategy {
    StateInArgs,
    StateInBuffer {
        lifecycle: BufferLifecycle,
    },
    DiffInArgs {
        base_account: Account,
    },
    DiffInBuffer {
        base_account: Account,
        lifecycle: BufferLifecycle,
    },
}

// CommitTask owns both "what to commit" (committed_account) and "how to commit" (strategy).
#[derive(Debug, Clone)]
pub struct CommitTask {
    pub commit_id: u64,
    pub allow_undelegation: bool,
    pub committed_account: CommittedAccount,
    pub strategy: CommitStrategy,
}

impl CommitTask {
    // Accounts larger than COMMIT_STATE_SIZE_THRESHOLD, use CommitDiff to
    // reduce instruction size. Below this, commit is sent as CommitState.
    // Chose 256 as thresold seems good enough as it could hold 8 u32 fields
    // or 4 u64 fields!
    pub const COMMIT_STATE_SIZE_THRESHOLD: usize = 256;

    pub async fn new<C: TaskInfoFetcher>(
        commit_id: u64,
        allow_undelegation: bool,
        committed_account: CommittedAccount,
        task_info_fetcher: &Arc<C>,
    ) -> Self {
        let base_account = if committed_account.account.data.len()
            > CommitTask::COMMIT_STATE_SIZE_THRESHOLD
        {
            match task_info_fetcher
                .get_base_account(&committed_account.pubkey)
                .await
            {
                Ok(Some(account)) => Some(account),
                Ok(None) => {
                    log::warn!("AccountNotFound for commit_diff, pubkey: {}, commit_id: {}, Falling back to commit_state.",
                        committed_account.pubkey, commit_id);
                    None
                }
                Err(e) => {
                    log::warn!("Failed to fetch base account for commit diff, pubkey: {}, commit_id: {}, error: {}. Falling back to commit_state.",
                        committed_account.pubkey, commit_id, e);
                    None
                }
            }
        } else {
            None
        };

        Self {
            commit_id,
            allow_undelegation,
            committed_account,
            strategy: match base_account {
                Some(base_account) => {
                    CommitStrategy::DiffInArgs { base_account }
                }
                None => CommitStrategy::StateInArgs,
            },
        }
    }

    pub fn lifecycle(&self) -> Option<&BufferLifecycle> {
        match &self.strategy {
            CommitStrategy::StateInArgs => None,
            CommitStrategy::StateInBuffer { lifecycle } => Some(&lifecycle),
            CommitStrategy::DiffInArgs { base_account: _ } => None,
            CommitStrategy::DiffInBuffer {
                lifecycle,
                base_account: _,
            } => Some(&lifecycle),
        }
    }

    pub fn task_strategy(&self) -> TaskStrategy {
        match &self.strategy {
            CommitStrategy::StateInArgs => TaskStrategy::Args,
            CommitStrategy::StateInBuffer { .. } => TaskStrategy::Buffer,
            CommitStrategy::DiffInArgs { base_account: _ } => {
                TaskStrategy::Args
            }
            CommitStrategy::DiffInBuffer { .. } => TaskStrategy::Buffer,
        }
    }

    pub fn reset_commit_id(&mut self, commit_id: u64) {
        if self.commit_id == commit_id {
            return;
        }

        self.commit_id = commit_id;
        let lifecycle = match &mut self.strategy {
            CommitStrategy::StateInArgs => None,
            CommitStrategy::StateInBuffer { lifecycle } => Some(lifecycle),
            CommitStrategy::DiffInArgs { base_account: _ } => None,
            CommitStrategy::DiffInBuffer {
                base_account: _,
                lifecycle,
            } => Some(lifecycle),
        };

        if let Some(lifecycle) = lifecycle {
            lifecycle.preparation.commit_id = commit_id;
            lifecycle.cleanup.commit_id = commit_id;
        }
    }

    pub fn create_commit_ix(&self, validator: &Pubkey) -> Instruction {
        match &self.strategy {
            CommitStrategy::StateInArgs => {
                self.create_commit_state_ix(validator)
            }
            CommitStrategy::StateInBuffer { lifecycle: _ } => {
                self.create_commit_state_from_buffer_ix(validator)
            }
            CommitStrategy::DiffInArgs { base_account } => {
                self.create_commit_diff_ix(validator, base_account)
            }
            CommitStrategy::DiffInBuffer {
                base_account: _,
                lifecycle: _,
            } => self.create_commit_diff_from_buffer_ix(validator),
        }
    }

    // FIXME: DELETE
    pub fn compute_diff(&self) -> Option<dlp::rkyv::AlignedVec> {
        panic!()
    }

    fn create_commit_state_ix(&self, validator: &Pubkey) -> Instruction {
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

    fn create_commit_diff_ix(
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

    fn create_commit_state_from_buffer_ix(
        &self,
        validator: &Pubkey,
    ) -> Instruction {
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

    fn create_commit_diff_from_buffer_ix(
        &self,
        validator: &Pubkey,
    ) -> Instruction {
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

    ///
    /// In order to reduce the transition size, this function
    /// flips *_InArgs to *_InBuffer and attach a LifecycleTask.
    ///
    pub fn try_optimize_tx_size(mut self) -> Result<CommitTask, CommitTask> {
        // The only way to optimize for tx size is to use buffer strategy.
        // If the task is already using buffer strategy, then it cannot optimize further.
        match self.strategy {
            CommitStrategy::StateInArgs => {
                self.strategy = CommitStrategy::StateInBuffer {
                    lifecycle: BufferLifecycle::new(
                        self.commit_id,
                        &self.committed_account,
                        None,
                    ),
                };
                Ok(self)
            }
            CommitStrategy::StateInBuffer { .. } => Err(self),
            CommitStrategy::DiffInArgs { base_account } => {
                self.strategy = CommitStrategy::DiffInBuffer {
                    lifecycle: BufferLifecycle::new(
                        self.commit_id,
                        &self.committed_account,
                        Some(&base_account),
                    ),
                    base_account,
                };
                Ok(self)
            }
            CommitStrategy::DiffInBuffer { .. } => Err(self),
        }
    }
}
