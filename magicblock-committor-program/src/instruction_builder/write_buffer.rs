use solana_program::instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use crate::{
    instruction::CommittorInstruction, instruction_builder::build_instruction,
    pdas,
};

// -----------------
// create_write_ix
// -----------------
pub struct CreateWriteIxArgs {
    pub authority: Pubkey,
    pub pubkey: Pubkey,
    pub offset: u32,
    pub data_chunk: Vec<u8>,
    pub commit_id: u64,
}

pub fn create_write_ix(args: CreateWriteIxArgs) -> Instruction {
    let CreateWriteIxArgs {
        authority,
        pubkey,
        offset,
        data_chunk,
        commit_id,
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

    let ix = CommittorInstruction::Write {
        pubkey,
        commit_id,
        chunks_bump,
        buffer_bump,
        offset,
        data_chunk,
    };
    let accounts = vec![
        AccountMeta::new(authority, true),
        AccountMeta::new(chunks_pda, false),
        AccountMeta::new(buffer_pda, false),
    ];
    build_instruction(ix, accounts)
}
