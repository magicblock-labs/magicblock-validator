use borsh::BorshSerialize;
use solana_program::instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use crate::{
    consts::MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE,
    instruction::CommittorInstruction, pdas,
};

// -----------------
// create_realloc_buffer_ix
// -----------------
#[derive(Clone)]
pub struct CreateReallocBufferIxArgs {
    pub authority: Pubkey,
    pub pubkey: Pubkey,
    pub buffer_account_size: u64,
    pub commit_id: u64,
}

/// Creates the realloc ixs we need to invoke in order to realloc
/// the account to the desired size since we only can realloc up to
/// [consts::MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE] in a single instruction.
/// Returns a tuple with the instructions and a bool indicating if we need to split
/// them into multiple instructions in order to avoid
/// [solana_program::program_error::MAX_INSTRUCTION_TRACE_LENGTH_EXCEEDED]J
pub fn create_realloc_buffer_ixs(
    args: CreateReallocBufferIxArgs,
) -> Vec<Instruction> {
    // We already allocated once during Init and only need to realloc
    // if the buffer is larger than [consts::MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE]
    if args.buffer_account_size <= MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE as u64
    {
        return vec![];
    }

    // Use remaining since [`MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE`] allocated at init
    let remaining_size = args.buffer_account_size
        - MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE as u64;

    // B) We need to realloc multiple times
    // SAFETY; remaining size > consts::MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE
    create_realloc_buffer_ixs_to_add_remaining(&args, remaining_size)
}

pub fn create_realloc_buffer_ixs_to_add_remaining(
    args: &CreateReallocBufferIxArgs,
    remaining_size: u64,
) -> Vec<Instruction> {
    let remaining_invocation_count =
        remaining_size.div_ceil(MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE as u64);
    // Generate one instruction per needed allocation
    (1..=remaining_invocation_count)
        .map(|i| create_realloc_buffer_ix(args.clone(), i as u16))
        .collect()
}

fn create_realloc_buffer_ix(
    args: CreateReallocBufferIxArgs,
    invocation_count: u16,
) -> Instruction {
    let CreateReallocBufferIxArgs {
        authority,
        pubkey,
        buffer_account_size,
        commit_id,
    } = args;
    let (buffer_pda, buffer_bump) = pdas::buffer_pda(
        &authority,
        &pubkey,
        commit_id.to_le_bytes().as_slice(),
    );

    let program_id = crate::id();
    let ix = CommittorInstruction::ReallocBuffer {
        pubkey,
        buffer_account_size,
        commit_id,
        buffer_bump,
        invocation_count,
    };
    let accounts = vec![
        AccountMeta::new(authority, true),
        AccountMeta::new(buffer_pda, false),
    ];
    Instruction::new_with_bytes(program_id, &ix.try_to_vec().unwrap(), accounts)
}
