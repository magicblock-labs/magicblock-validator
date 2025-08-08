use solana_program::{
    instruction::{AccountMeta, Instruction},
    system_program,
};
use solana_pubkey::Pubkey;

use crate::{instruction::CommittorInstruction, pdas};

// -----------------
// create_init_ix
// -----------------
pub struct CreateInitIxArgs {
    /// The validator authority
    pub authority: Pubkey,
    /// On chain address of the account we are committing
    pub pubkey: Pubkey,
    /// Required size of the account tracking which chunks have been committed
    pub chunks_account_size: u64,
    /// Required size of the buffer account that holds the account data to commit
    pub buffer_account_size: u64,
    /// The commit_id account commitments associated with
    pub commit_id: u64,
    /// The number of chunks we need to write until all the data is copied to the
    /// buffer account
    pub chunk_count: usize,
    /// The size of each chunk that we write to the buffer account
    pub chunk_size: u16,
}

pub fn create_init_ix(args: CreateInitIxArgs) -> (Instruction, Pubkey, Pubkey) {
    let CreateInitIxArgs {
        authority,
        pubkey,
        chunks_account_size,
        buffer_account_size,
        commit_id,
        chunk_count,
        chunk_size,
    } = args;

    let (chunks_pda, chunks_bump) = pdas::chunks_pda(
        &authority,
        &pubkey,
        commit_id.to_le_bytes().as_slice(),
    );
    let (buffer_pda, buffer_bump) = pdas::buffer_pda(
        &authority,
        &pubkey,
        commit_id.to_le_bytes().as_slice(),
    );
    let program_id = crate::id();
    let ix = CommittorInstruction::Init {
        pubkey,
        commit_id,
        chunks_account_size,
        buffer_account_size,
        chunks_bump,
        buffer_bump,
        chunk_count,
        chunk_size,
    };
    let accounts = vec![
        AccountMeta::new(authority, true),
        AccountMeta::new(chunks_pda, false),
        AccountMeta::new(buffer_pda, false),
        AccountMeta::new_readonly(system_program::id(), false),
    ];
    (
        Instruction::new_with_borsh(program_id, &ix, accounts),
        chunks_pda,
        buffer_pda,
    )
}
