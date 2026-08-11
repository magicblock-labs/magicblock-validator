use dlp_api::{args::PreallocateBufferKind, diff::compute_diff};
use magicblock_committor_program::{
    instruction_builder::{
        close_buffer::{create_close_ix, CreateCloseIxArgs},
        init_buffer::{create_init_ix, CreateInitIxArgs},
        realloc_buffer::{
            create_realloc_buffer_ixs, CreateReallocBufferIxArgs,
        },
        write_buffer::{create_write_ix, CreateWriteIxArgs},
    },
    pdas, ChangesetChunks, Chunks,
};
use magicblock_program::Pubkey;
use solana_instruction::Instruction;
use solana_program::account_info::MAX_PERMITTED_DATA_INCREASE;

use crate::{
    consts::MAX_WRITE_CHUNK_SIZE,
    tasks::{
        commit_finalize_task::CommitFinalizeTask,
        commit_task::{CommitDelivery, CommitTask},
        BaseTaskImpl, FinalizeTask, UndelegateTask,
    },
};

#[derive(Debug)]
pub struct BufferPreparationTask<'a> {
    pub commit_id: u64,
    pub pubkey: Pubkey,
    pub chunks: Chunks,
    buffer_data: Vec<u8>,
    prepared: &'a mut bool,
}

impl<'a> BufferPreparationTask<'a> {
    pub fn from_commit(task: &'a mut CommitTask) -> Option<Self> {
        match &mut task.delivery_details {
            CommitDelivery::StateInArgs | CommitDelivery::DiffInArgs { .. } => {
                None
            }
            CommitDelivery::StateInBuffer { prepared } => {
                let buffer_data = task.committed_account.account.data.clone();
                let chunks = Chunks::from_data_length(
                    buffer_data.len(),
                    MAX_WRITE_CHUNK_SIZE,
                );
                Some(Self {
                    commit_id: task.commit_id,
                    pubkey: task.committed_account.pubkey,
                    buffer_data,
                    chunks,
                    prepared,
                })
            }
            CommitDelivery::DiffInBuffer {
                base_account,
                prepared,
            } => {
                let diff = compute_diff(
                    base_account.data.as_ref(),
                    &task.committed_account.account.data,
                )
                .to_vec();
                let chunks =
                    Chunks::from_data_length(diff.len(), MAX_WRITE_CHUNK_SIZE);
                Some(Self {
                    commit_id: task.commit_id,
                    pubkey: task.committed_account.pubkey,
                    buffer_data: diff,
                    chunks,
                    prepared,
                })
            }
        }
    }

    pub fn from_commit_finalize(
        task: &'a mut CommitFinalizeTask,
    ) -> Option<Self> {
        match &mut task.delivery {
            CommitDelivery::StateInArgs | CommitDelivery::DiffInArgs { .. } => {
                None
            }
            CommitDelivery::StateInBuffer { prepared } => {
                let buffer_data = task.committed_account.account.data.clone();
                let chunks = Chunks::from_data_length(
                    buffer_data.len(),
                    MAX_WRITE_CHUNK_SIZE,
                );
                Some(Self {
                    commit_id: task.commit_id,
                    pubkey: task.committed_account.pubkey,
                    buffer_data,
                    chunks,
                    prepared,
                })
            }
            CommitDelivery::DiffInBuffer {
                base_account,
                prepared,
            } => {
                let diff = compute_diff(
                    base_account.data.as_ref(),
                    &task.committed_account.account.data,
                )
                .to_vec();
                let chunks =
                    Chunks::from_data_length(diff.len(), MAX_WRITE_CHUNK_SIZE);
                Some(Self {
                    commit_id: task.commit_id,
                    pubkey: task.committed_account.pubkey,
                    buffer_data: diff,
                    chunks,
                    prepared,
                })
            }
        }
    }

    /// Returns initialization [`Instruction`]
    pub fn init_instruction(&self, authority: &Pubkey) -> Instruction {
        // // SAFETY: as object_length internally uses only already allocated or static buffers,
        // // and we don't use any fs writers, so the only error that may occur here is of kind
        // // OutOfMemory or WriteZero. This is impossible due to:
        // // Chunks::new panics if its size exceeds MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE or 10_240
        // // https://github.com/near/borsh-rs/blob/f1b75a6b50740bfb6231b7d0b1bd93ea58ca5452/borsh/src/ser/helpers.rs#L59
        let chunks_account_size =
            borsh::object_length(&self.chunks).unwrap() as u64;
        let buffer_account_size = self.buffer_data.len() as u64;

        let (instruction, _, _) = create_init_ix(CreateInitIxArgs {
            authority: *authority,
            pubkey: self.pubkey,
            chunks_account_size,
            buffer_account_size,
            commit_id: self.commit_id,
            chunk_count: self.chunks.count(),
            chunk_size: self.chunks.chunk_size(),
        });

        instruction
    }

    /// Returns compute units required for realloc instruction
    pub fn init_compute_units(&self) -> u32 {
        12_000
    }

    /// Returns realloc instruction required for Buffer preparation
    #[allow(clippy::let_and_return)]
    pub fn realloc_instructions(&self, authority: &Pubkey) -> Vec<Instruction> {
        let buffer_account_size = self.buffer_data.len() as u64;
        let realloc_instructions =
            create_realloc_buffer_ixs(CreateReallocBufferIxArgs {
                authority: *authority,
                pubkey: self.pubkey,
                buffer_account_size,
                commit_id: self.commit_id,
            });

        realloc_instructions
    }

    /// Returns compute units required for realloc instruction
    pub fn realloc_compute_units(&self) -> u32 {
        6_000
    }

    /// Returns realloc instruction required for Buffer preparation
    #[allow(clippy::let_and_return)]
    pub fn write_instructions(&self, authority: &Pubkey) -> Vec<Instruction> {
        let chunks_iter =
            ChangesetChunks::new(&self.chunks, self.chunks.chunk_size())
                .iter(&self.buffer_data);
        let write_instructions = chunks_iter
            .map(|chunk| {
                create_write_ix(CreateWriteIxArgs {
                    authority: *authority,
                    pubkey: self.pubkey,
                    offset: chunk.offset,
                    data_chunk: chunk.data_chunk,
                    commit_id: self.commit_id,
                })
            })
            .collect::<Vec<_>>();

        write_instructions
    }

    pub fn write_compute_units(&self, bytes_count: usize) -> u32 {
        const PER_BYTE: u32 = 3;

        u32::try_from(bytes_count)
            .ok()
            .and_then(|bytes_count| bytes_count.checked_mul(PER_BYTE))
            .unwrap_or(u32::MAX)
    }

    pub fn chunks_pda(&self, authority: &Pubkey) -> Pubkey {
        pdas::chunks_pda(
            authority,
            &self.pubkey,
            self.commit_id.to_le_bytes().as_slice(),
        )
        .0
    }

    pub fn buffer_pda(&self, authority: &Pubkey) -> Pubkey {
        pdas::buffer_pda(
            authority,
            &self.pubkey,
            self.commit_id.to_le_bytes().as_slice(),
        )
        .0
    }

    pub fn cleanup_task(&self) -> CleanupTask {
        CleanupTask {
            pubkey: self.pubkey,
            commit_id: self.commit_id,
        }
    }

    pub fn done(self) {
        *self.prepared = true;
    }
}

#[derive(Clone, Debug)]
pub struct CleanupTask {
    pub pubkey: Pubkey,
    pub commit_id: u64,
}

impl CleanupTask {
    pub fn from_commit(task: &CommitTask) -> Option<Self> {
        match &task.delivery_details {
            CommitDelivery::StateInBuffer { prepared: true }
            | CommitDelivery::DiffInBuffer { prepared: true, .. } => {
                Some(Self {
                    commit_id: task.commit_id,
                    pubkey: task.committed_account.pubkey,
                })
            }
            _ => None,
        }
    }

    pub fn from_commit_finalize(task: &CommitFinalizeTask) -> Option<Self> {
        match &task.delivery {
            CommitDelivery::StateInBuffer { prepared: true }
            | CommitDelivery::DiffInBuffer { prepared: true, .. } => {
                Some(Self {
                    commit_id: task.commit_id,
                    pubkey: task.committed_account.pubkey,
                })
            }
            _ => None,
        }
    }

    pub fn instruction(&self, authority: &Pubkey) -> Instruction {
        create_close_ix(CreateCloseIxArgs {
            authority: *authority,
            pubkey: self.pubkey,
            commit_id: self.commit_id,
        })
    }

    /// Returns compute units required to execute [`crate::tasks::CleanupTask`]
    pub fn compute_units(&self) -> u32 {
        30_000
    }

    /// Returns a number of [`CleanupTask`]s that is possible to fit in single
    pub const fn max_tx_fit_count_with_budget() -> usize {
        8
    }

    pub fn chunks_pda(&self, authority: &Pubkey) -> Pubkey {
        pdas::chunks_pda(
            authority,
            &self.pubkey,
            self.commit_id.to_le_bytes().as_slice(),
        )
        .0
    }

    pub fn buffer_pda(&self, authority: &Pubkey) -> Pubkey {
        pdas::buffer_pda(
            authority,
            &self.pubkey,
            self.commit_id.to_le_bytes().as_slice(),
        )
        .0
    }
}

/// TODO(edwin):
/// Could be implemented by CommitTask, FinalizeTask and others
pub trait HasPreparation<P> {
    // Returns Some(P) if task requires preparation
    fn preparation_task(&self) -> Option<P>;
}

pub struct PreallocateCommitStateTask {
    delegated_account: Pubkey,
    target_size: u32,
}

impl PreallocateCommitStateTask {
    pub fn from(task: &BaseTaskImpl) -> Option<Self> {
        match &task {
            BaseTaskImpl::Commit(value) => Self::from_commit(value),
            _ => None,
        }
    }

    pub fn from_commit(task: &CommitTask) -> Option<Self> {
        let delegated_account = task.committed_account.pubkey;
        match &task.delivery_details {
            // We assume that tx can't carry over 10kb in arguments
            CommitDelivery::StateInArgs | CommitDelivery::DiffInArgs { .. } => {
                None
            }
            CommitDelivery::StateInBuffer { .. }
            // commit_state is always written with the full reconstructed
            // account state, so the target size is the same regardless of
            // whether delivery is diff or state based. see dlp merge_diff_copy
            | CommitDelivery::DiffInBuffer { .. } => {
                let target_size = task.committed_account.account.data.len();
                if target_size > MAX_PERMITTED_DATA_INCREASE {
                    Some(Self {
                        delegated_account,
                        target_size: target_size as u32,
                    })
                } else {
                    None
                }
            }
        }
    }

    /// Returns instructions for allocating the commit_state buffer.
    pub fn instructions(&self, validator: &Pubkey) -> Vec<Instruction> {
        dlp_api::instruction_builder::preallocate_buffer_chunks(
            *validator,
            self.delegated_account,
            PreallocateBufferKind::CommitState,
            0,
            self.target_size,
        )
    }
}

pub struct PreallocateCommitFinalizeTask {
    delegated_account: Pubkey,
    target_size: u32,
}

impl PreallocateCommitFinalizeTask {
    pub fn from(task: &BaseTaskImpl) -> Option<Self> {
        match &task {
            BaseTaskImpl::CommitFinalize(value) => {
                Self::from_commit_finalize(value)
            }
            _ => None,
        }
    }

    pub fn from_commit_finalize(task: &CommitFinalizeTask) -> Option<Self> {
        let delegated_account = task.committed_account.pubkey;
        match &task.delivery {
            // We assume that tx can't carry over 10kb in arguments
            CommitDelivery::StateInArgs | CommitDelivery::DiffInArgs { .. } => {
                None
            }
            // Same reasoning as PreallocateCommitStateTask::from_commit
            CommitDelivery::StateInBuffer { .. }
            | CommitDelivery::DiffInBuffer { .. } => {
                let target_size = task.committed_account.account.data.len();
                if target_size > MAX_PERMITTED_DATA_INCREASE {
                    Some(Self {
                        delegated_account,
                        target_size: target_size as u32,
                    })
                } else {
                    None
                }
            }
        }
    }

    /// Returns instructions for allocating the delegated account.
    pub fn instructions(&self, validator: &Pubkey) -> Vec<Instruction> {
        dlp_api::instruction_builder::preallocate_buffer_chunks(
            *validator,
            self.delegated_account,
            PreallocateBufferKind::DelegatedAccount,
            0,
            self.target_size,
        )
    }
}

pub struct PreallocateFinalizeTask {
    delegated_account: Pubkey,
    target_size: u32,
}

impl PreallocateFinalizeTask {
    pub fn from(task: &BaseTaskImpl) -> Option<Self> {
        match &task {
            BaseTaskImpl::Finalize(value) => Self::from_finalize_task(value),
            _ => None,
        }
    }

    pub fn from_finalize_task(task: &FinalizeTask) -> Option<Self> {
        (task.state_size > MAX_PERMITTED_DATA_INCREASE).then_some(Self {
            delegated_account: task.delegated_account,
            target_size: task.state_size as u32,
        })
    }

    /// Returns instructions for allocating the delegated account.
    pub fn instructions(&self, validator: &Pubkey) -> Vec<Instruction> {
        dlp_api::instruction_builder::preallocate_buffer_chunks(
            *validator,
            self.delegated_account,
            PreallocateBufferKind::DelegatedAccount,
            0,
            self.target_size,
        )
    }
}

pub struct PreallocateUndelegateTask {
    delegated_account: Pubkey,
    target_size: u32,
}

impl PreallocateUndelegateTask {
    pub fn from(task: &BaseTaskImpl) -> Option<Self> {
        match &task {
            BaseTaskImpl::Undelegate(value) => {
                Self::from_undelegate_task(value)
            }
            _ => None,
        }
    }

    pub fn from_undelegate_task(task: &UndelegateTask) -> Option<Self> {
        (task.state_size > MAX_PERMITTED_DATA_INCREASE).then_some(Self {
            delegated_account: task.delegated_account,
            target_size: task.state_size as u32,
        })
    }

    /// Returns instructions for allocating the undelegate buffer.
    pub fn instructions(&self, validator: &Pubkey) -> Vec<Instruction> {
        dlp_api::instruction_builder::preallocate_buffer_chunks(
            *validator,
            self.delegated_account,
            PreallocateBufferKind::UndelegateBuffer,
            0,
            self.target_size,
        )
    }
}

#[cfg(test)]
mod tests {
    use solana_hash::Hash;
    use solana_keypair::Keypair;
    use solana_message::{v0::Message, VersionedMessage};
    use solana_pubkey::Pubkey;
    use solana_signer::Signer;
    use solana_transaction::versioned::VersionedTransaction;
    use tracing::info;

    use super::*;
    use crate::{
        tasks::utils::TransactionUtils,
        test_utils,
        transactions::{
            serialized_transaction_size, MAX_TRANSACTION_WIRE_SIZE,
        },
        ComputeBudgetConfig,
    };

    #[test]
    fn test_max_write_with_uniqueness_nonce_fits() {
        test_utils::init_test_logger();

        let authority = Keypair::new();
        let data = vec![0; MAX_WRITE_CHUNK_SIZE as usize];
        let mut prepared = false;
        let preparation_task = BufferPreparationTask {
            commit_id: 1,
            pubkey: Pubkey::new_unique(),
            chunks: Chunks::from_data_length(data.len(), MAX_WRITE_CHUNK_SIZE),
            buffer_data: data,
            prepared: &mut prepared,
        };
        let write_instruction = preparation_task
            .write_instructions(&authority.pubkey())
            .into_iter()
            .next()
            .expect("write instruction");
        let mut instructions = ComputeBudgetConfig::new(1_000_000)
            .buffer_write
            .instructions(write_instruction.data.len());
        instructions.push(write_instruction);
        instructions.push(TransactionUtils::uniqueness_noop_instruction(42));

        let message = Message::try_compile(
            &authority.pubkey(),
            &instructions,
            &[],
            Hash::new_unique(),
        )
        .expect("compile write transaction");
        let transaction = VersionedTransaction::try_new(
            VersionedMessage::V0(message),
            &[&authority],
        )
        .expect("sign write transaction");
        let transaction_size = serialized_transaction_size(&transaction);
        info!(transaction_size, "Buffer write transaction size");
        assert!(transaction_size <= MAX_TRANSACTION_WIRE_SIZE);
    }
}
