use committor_interface::{
    ChangesetChunks, Chunks,
    instruction_builder::{
        close_buffer::{CreateCloseIxArgs, create_close_ix},
        init_buffer::{CreateInitIxArgs, create_init_ix},
        realloc_buffer::{CreateReallocBufferIxArgs, create_realloc_buffer_ixs},
        write_buffer::{CreateWriteIxArgs, create_write_ix},
    },
    pdas,
};
use dpi::diff::compute_diff;
use program::Pubkey;
use solana_instruction::Instruction;

use crate::shared::{
    tasks::{commit_delivery::CommitDelivery, commit_finalize_task::CommitFinalizeTask},
    transaction::MAX_WRITE_CHUNK_SIZE,
};

#[derive(Debug)]
pub(crate) struct BufferPreparation {
    pub commit_id: u64,
    pub pubkey: Pubkey,
    pub chunks: Chunks,
    buffer_data: Vec<u8>,
}

impl BufferPreparation {
    pub(crate) fn from_commit_finalize(task: &CommitFinalizeTask) -> Option<Self> {
        match &task.delivery {
            CommitDelivery::StateInArgs | CommitDelivery::DiffInArgs { .. } => None,
            CommitDelivery::StateInBuffer => {
                let buffer_data = task.committed_account.account.data.clone();
                let chunks = Chunks::from_data_length(buffer_data.len(), MAX_WRITE_CHUNK_SIZE);
                Some(Self {
                    commit_id: task.commit_id,
                    pubkey: task.committed_account.pubkey,
                    buffer_data,
                    chunks,
                })
            }
            CommitDelivery::DiffInBuffer { base_account } => {
                let diff = compute_diff(
                    base_account.data.as_ref(),
                    &task.committed_account.account.data,
                )
                .to_vec();
                let chunks = Chunks::from_data_length(diff.len(), MAX_WRITE_CHUNK_SIZE);
                Some(Self {
                    commit_id: task.commit_id,
                    pubkey: task.committed_account.pubkey,
                    buffer_data: diff,
                    chunks,
                })
            }
        }
    }

    /// Returns initialization [`Instruction`]
    pub(crate) fn init_instruction(&self, authority: &Pubkey) -> Instruction {
        let chunks_account_size = self.chunks.serialized_size() as u64;
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

    /// Returns realloc instruction required for Buffer preparation
    pub(crate) fn realloc_instructions(&self, authority: &Pubkey) -> Vec<Instruction> {
        let buffer_account_size = self.buffer_data.len() as u64;
        create_realloc_buffer_ixs(CreateReallocBufferIxArgs {
            authority: *authority,
            pubkey: self.pubkey,
            buffer_account_size,
            commit_id: self.commit_id,
        })
    }

    /// Returns instructions that write the buffer data in chunks.
    pub(crate) fn write_instructions(&self, authority: &Pubkey) -> Vec<Instruction> {
        let chunks_iter =
            ChangesetChunks::new(&self.chunks, self.chunks.chunk_size()).iter(&self.buffer_data);
        chunks_iter
            .map(|chunk| {
                create_write_ix(CreateWriteIxArgs {
                    authority: *authority,
                    pubkey: self.pubkey,
                    offset: chunk.offset,
                    data_chunk: chunk.data_chunk,
                    commit_id: self.commit_id,
                })
            })
            .collect()
    }

    pub(crate) fn cleanup_task(&self) -> BufferCleanup {
        BufferCleanup {
            pubkey: self.pubkey,
            commit_id: self.commit_id,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct BufferCleanup {
    pub pubkey: Pubkey,
    pub commit_id: u64,
}

impl BufferCleanup {
    pub(crate) fn from_commit_finalize(task: &CommitFinalizeTask) -> Option<Self> {
        match &task.delivery {
            CommitDelivery::StateInBuffer | CommitDelivery::DiffInBuffer { .. } => Some(Self {
                commit_id: task.commit_id,
                pubkey: task.committed_account.pubkey,
            }),
            _ => None,
        }
    }

    pub(crate) fn instruction(&self, authority: &Pubkey) -> Instruction {
        create_close_ix(CreateCloseIxArgs {
            authority: *authority,
            pubkey: self.pubkey,
            commit_id: self.commit_id,
        })
    }

    /// Compute units required to close a buffer and its chunks account.
    pub(crate) const COMPUTE_UNITS: u32 = 30_000;

    /// Maximum number of buffer cleanup instructions that fit in one transaction.
    pub(crate) const MAX_PER_TRANSACTION: usize = 8;

    pub(crate) fn buffer_pda(&self, authority: &Pubkey) -> Pubkey {
        pdas::buffer_pda(
            authority,
            &self.pubkey,
            self.commit_id.to_le_bytes().as_slice(),
        )
        .0
    }
}

#[cfg(test)]
mod tests {
    use solana_hash::Hash;
    use solana_keypair::Keypair;
    use solana_message::{VersionedMessage, v0::Message};
    use solana_pubkey::Pubkey;
    use solana_signer::Signer;
    use solana_transaction::versioned::VersionedTransaction;
    use tracing::info;

    use super::*;
    use crate::{
        ComputeBudgetConfig,
        shared::{
            tasks::utils::TransactionUtils,
            transaction::{MAX_TRANSACTION_WIRE_SIZE, serialized_transaction_size},
        },
        test_utils,
    };

    #[test]
    fn test_max_write_with_uniqueness_nonce_fits() {
        test_utils::init_test_logger();

        let authority = Keypair::new();
        let data = vec![0; MAX_WRITE_CHUNK_SIZE as usize];
        let preparation_task = BufferPreparation {
            commit_id: 1,
            pubkey: Pubkey::new_unique(),
            chunks: Chunks::from_data_length(data.len(), MAX_WRITE_CHUNK_SIZE),
            buffer_data: data,
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

        let message =
            Message::try_compile(&authority.pubkey(), &instructions, &[], Hash::new_unique())
                .expect("compile write transaction");
        let transaction =
            VersionedTransaction::try_new(VersionedMessage::V0(message), &[&authority])
                .expect("sign write transaction");
        let transaction_size = serialized_transaction_size(&transaction);
        info!(transaction_size, "Buffer write transaction size");
        assert!(transaction_size <= MAX_TRANSACTION_WIRE_SIZE);
    }
}
