use solana_program::instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use crate::{
    instruction::CommittorInstruction, instruction_builder::build_instruction,
    pdas,
};

// -----------------
// create_close_ix
// -----------------
pub struct CreateCloseIxArgs {
    pub authority: Pubkey,
    pub pubkey: Pubkey,
    pub commit_id: u64,
}

pub fn create_close_ix(args: CreateCloseIxArgs) -> Instruction {
    let CreateCloseIxArgs {
        authority,
        pubkey,
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

    let ix = CommittorInstruction::Close {
        pubkey,
        commit_id,
        chunks_bump,
        buffer_bump,
    };
    let accounts = vec![
        AccountMeta::new(authority, true),
        AccountMeta::new(chunks_pda, false),
        AccountMeta::new(buffer_pda, false),
    ];
    build_instruction(ix, accounts)
}
