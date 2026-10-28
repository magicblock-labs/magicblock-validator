use committor_interface::{
    Chunks,
    instruction_builder::{
        close_buffer::{CreateCloseIxArgs, create_close_ix},
        init_buffer::{CreateInitIxArgs, create_init_ix},
        realloc_buffer::{CreateReallocBufferIxArgs, create_realloc_buffer_ixs},
        write_buffer::{CreateWriteIxArgs, create_write_ix},
    },
    pdas,
};
use delegation_program_interface::{self as dpi, diff::compute_diff};
use program::Pubkey;
use solana_instruction::Instruction;
use wheels::DataLayoutError;

use crate::shared::{
    account_state::DelegationProtocol,
    tasks::{commit_delivery::CommitDelivery, commit_finalize_task::CommitFinalizeTask},
    transaction::MAX_WRITE_CHUNK_SIZE,
};

#[derive(Debug)]
pub(crate) struct BufferPreparation {
    pub protocol: DelegationProtocol,
    pub commit_id: u64,
    pub pubkey: Pubkey,
    pub chunks: Chunks,
    pub buffer_data: Vec<u8>,
}

impl BufferPreparation {
    pub(crate) fn from_commit_finalize(task: &CommitFinalizeTask) -> Option<Self> {
        let buffer_data = match &task.delivery {
            CommitDelivery::StateInArgs | CommitDelivery::DiffInArgs { .. } => return None,
            CommitDelivery::StateInBuffer => task.committed_account.account.data.clone(),
            CommitDelivery::DiffInBuffer { base_account } => compute_diff(
                base_account.data.as_ref(),
                &task.committed_account.account.data,
            )
            .to_vec(),
        };
        let chunks = Chunks::from_data_length(buffer_data.len(), MAX_WRITE_CHUNK_SIZE);
        Some(Self {
            protocol: task.protocol,
            commit_id: task.commit_id,
            pubkey: task.committed_account.pubkey,
            buffer_data,
            chunks,
        })
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
    pub(crate) fn write_instructions(
        &self,
        authority: &Pubkey,
        start_offset: usize,
    ) -> Vec<Instruction> {
        let instruction = |offset: usize, chunk: &[u8]| match self.protocol {
            DelegationProtocol::Legacy => create_write_ix(CreateWriteIxArgs {
                authority: *authority,
                pubkey: self.pubkey,
                offset: offset as u32,
                data_chunk: chunk.to_vec(),
                commit_id: self.commit_id,
            }),
            DelegationProtocol::V2 { .. } => dpi::instruction_builder::write_state_buffer(
                *authority,
                *authority,
                self.pubkey,
                dpi::WriteStateBufferArgs {
                    commit_id: self.commit_id,
                    total_len: self.buffer_data.len() as u32,
                    offset: offset as u32,
                    chunk: chunk.to_vec(),
                },
            ),
        };
        if self.buffer_data.is_empty() && matches!(self.protocol, DelegationProtocol::V2 { .. }) {
            return vec![instruction(0, &[])];
        }
        self.buffer_data[start_offset..]
            .chunks(MAX_WRITE_CHUNK_SIZE as usize)
            .enumerate()
            .map(|(index, chunk)| {
                instruction(start_offset + index * MAX_WRITE_CHUNK_SIZE as usize, chunk)
            })
            .collect()
    }

    pub(crate) fn cleanup_task(&self) -> BufferCleanup {
        BufferCleanup {
            protocol: self.protocol,
            pubkey: self.pubkey,
            commit_id: self.commit_id,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct BufferCleanup {
    pub protocol: DelegationProtocol,
    pub pubkey: Pubkey,
    pub commit_id: u64,
}

impl BufferCleanup {
    pub(crate) fn from_commit_finalize(task: &CommitFinalizeTask) -> Option<Self> {
        match &task.delivery {
            CommitDelivery::StateInBuffer | CommitDelivery::DiffInBuffer { .. } => Some(Self {
                protocol: task.protocol,
                commit_id: task.commit_id,
                pubkey: task.committed_account.pubkey,
            }),
            _ => None,
        }
    }

    pub(crate) fn instruction(&self, authority: &Pubkey) -> Result<Instruction, DataLayoutError> {
        if matches!(self.protocol, DelegationProtocol::V2 { .. }) {
            // The committor funds its own buffers, so it is also the stored rent payer.
            return dpi::instruction_builder::close_state_buffer(
                *authority,
                self.pubkey,
                self.commit_id,
                *authority,
            );
        }
        Ok(create_close_ix(CreateCloseIxArgs {
            authority: *authority,
            pubkey: self.pubkey,
            commit_id: self.commit_id,
        }))
    }

    /// Compute units required to close a buffer and its chunks account.
    pub(crate) const COMPUTE_UNITS: u32 = 30_000;

    /// Fits legacy, v2, and mixed cleanup batches, including a uniqueness nonce.
    pub(crate) const MAX_PER_TRANSACTION: usize = 8;

    pub(crate) fn buffer_pda(&self, authority: &Pubkey) -> Pubkey {
        if matches!(self.protocol, DelegationProtocol::V2 { .. }) {
            return dpi::state_buffer_pda(&self.pubkey, self.commit_id, authority);
        }
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

        for protocol in [
            DelegationProtocol::Legacy,
            DelegationProtocol::V2 { cluster: Pubkey::new_unique() },
        ] {
            let authority = Keypair::new();
            let data = vec![0; MAX_WRITE_CHUNK_SIZE as usize];
            let preparation_task = BufferPreparation {
                protocol,
                commit_id: 1,
                pubkey: Pubkey::new_unique(),
                chunks: Chunks::from_data_length(data.len(), MAX_WRITE_CHUNK_SIZE),
                buffer_data: data,
            };
            let write_instruction = preparation_task
                .write_instructions(&authority.pubkey(), 0)
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
            assert!(
                transaction_size <= MAX_TRANSACTION_WIRE_SIZE,
                "{protocol:?}: {transaction_size}"
            );
        }
    }
}
