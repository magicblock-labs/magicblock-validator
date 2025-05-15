use borsh::{to_vec, BorshDeserialize};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, hash::Hash,
    log::sol_log_64, msg, program::invoke_signed, program_error::ProgramError,
    system_instruction, sysvar::Sysvar,
};
use solana_pubkey::Pubkey;

use crate::{
    consts,
    error::CommittorError,
    instruction::CommittorInstruction,
    utils::{
        assert_account_unallocated, assert_is_signer, assert_program_id,
        close_and_refund_authority,
    },
    verified_seeds_and_pda, Chunks,
};

pub fn process(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    assert_program_id(program_id)?;

    let ix = CommittorInstruction::try_from_slice(instruction_data)?;
    use CommittorInstruction::*;
    match ix {
        Init {
            pubkey,
            chunks_account_size,
            buffer_account_size,
            blockhash,
            chunks_bump,
            buffer_bump,
            chunk_count,
            chunk_size,
        } => process_init(
            program_id,
            accounts,
            &pubkey,
            chunks_account_size,
            buffer_account_size,
            blockhash,
            chunks_bump,
            buffer_bump,
            chunk_count,
            chunk_size,
        ),
        ReallocBuffer {
            pubkey,
            buffer_account_size,
            blockhash,
            buffer_bump,
            invocation_count,
        } => process_realloc_buffer(
            accounts,
            &pubkey,
            buffer_account_size,
            blockhash,
            buffer_bump,
            invocation_count,
        ),
        Write {
            pubkey,
            blockhash,
            chunks_bump,
            buffer_bump,
            offset,
            data_chunk,
        } => process_write(
            accounts,
            &pubkey,
            offset,
            data_chunk,
            blockhash,
            chunks_bump,
            buffer_bump,
        ),
        Close {
            pubkey,
            blockhash,
            chunks_bump,
            buffer_bump,
        } => process_close(
            accounts,
            &pubkey,
            blockhash,
            chunks_bump,
            buffer_bump,
        ),
    }
}

// -----------------
// process_init
// -----------------
#[allow(clippy::too_many_arguments)] // private + only call site is close
fn process_init(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    pubkey: &Pubkey,
    chunks_account_size: u64,
    buffer_account_size: u64,
    blockhash: Hash,
    chunks_bump: u8,
    buffer_bump: u8,
    chunk_count: usize,
    chunk_size: u16,
) -> ProgramResult {
    msg!("Instruction: Init");

    let [authority_info, chunks_account_info, buffer_account_info, _system_program] =
        accounts
    else {
        msg!("Need the following accounts: [authority, chunks, buffer, system program ], but got {}", accounts.len());
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    assert_is_signer(authority_info, "authority")?;

    let chunks_bump = &[chunks_bump];
    let (chunks_seeds, _chunks_pda) = verified_seeds_and_pda!(
        chunks,
        authority_info,
        pubkey,
        chunks_account_info,
        blockhash,
        chunks_bump
    );

    let buffer_bump = &[buffer_bump];
    let (buffer_seeds, _buffer_pda) = verified_seeds_and_pda!(
        buffer,
        authority_info,
        pubkey,
        buffer_account_info,
        blockhash,
        buffer_bump
    );

    assert_account_unallocated(chunks_account_info, "chunks")?;
    assert_account_unallocated(buffer_account_info, "buffer")?;

    msg!("Creating Chunks and Buffer accounts");

    // Create Chunks Account
    let ix = system_instruction::create_account(
        authority_info.key,
        chunks_account_info.key,
        solana_program::rent::Rent::get()?
            .minimum_balance(chunks_account_size as usize),
        chunks_account_size,
        program_id,
    );
    invoke_signed(
        &ix,
        &[authority_info.clone(), chunks_account_info.clone()],
        &[&chunks_seeds],
    )?;

    let initial_alloc_size = std::cmp::min(
        buffer_account_size,
        consts::MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE as u64,
    );

    // Create Buffer Account
    let ix = system_instruction::create_account(
        authority_info.key,
        buffer_account_info.key,
        // NOTE: we fund for the full size to allow realloc without funding more
        solana_program::rent::Rent::get()?
            .minimum_balance(buffer_account_size as usize),
        initial_alloc_size,
        program_id,
    );
    invoke_signed(
        &ix,
        &[authority_info.clone(), buffer_account_info.clone()],
        &[&buffer_seeds],
    )?;

    msg!(
        "Initialized and allocated {} of desired {} bytes.",
        initial_alloc_size,
        buffer_account_size,
    );

    // Initialize Chunks Account
    let chunks = Chunks::new(chunk_count, chunk_size);
    chunks_account_info
        .data
        .borrow_mut()
        .copy_from_slice(&to_vec(&chunks)?);

    Ok(())
}

// -----------------
// process_realloc_buffer
// -----------------
fn process_realloc_buffer(
    accounts: &[AccountInfo],
    pubkey: &Pubkey,
    buffer_account_size: u64,
    blockhash: Hash,
    buffer_bump: u8,
    invocation_count: u16,
) -> ProgramResult {
    msg!("Instruction: ReallocBuffer {}", invocation_count);

    let [authority_info, buffer_account_info] = accounts else {
        msg!(
            "Need the following accounts: [authority, buffer ], but got {}",
            accounts.len()
        );
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if buffer_account_info.data.borrow().len() >= buffer_account_size as usize {
        msg!(
            "Buffer account already has {} bytes, no need to realloc",
            buffer_account_info.data.borrow().len()
        );
        return Ok(());
    }

    assert_is_signer(authority_info, "authority")?;

    let buffer_bump = &[buffer_bump];
    verified_seeds_and_pda!(
        buffer,
        authority_info,
        pubkey,
        buffer_account_info,
        blockhash,
        buffer_bump
    );

    let current_buffer_size = buffer_account_info.data.borrow().len() as u64;
    let next_alloc_size = std::cmp::min(
        buffer_account_size,
        current_buffer_size
            + consts::MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE as u64,
    );

    msg!(
        "Allocating from {} to {} of desired {} bytes.",
        current_buffer_size,
        next_alloc_size,
        buffer_account_size,
    );

    // NOTE: we fund the account for the full desired account size during init
    //       Doing this as needed increases the cost for each realloc to 4,959 CUs.
    //       Reallocing without any rent check/increase uses only 4,025 CUs
    //       and does not require the system program to be provided.
    buffer_account_info.realloc(next_alloc_size as usize, true)?;

    Ok(())
}

// -----------------
// process_write
// -----------------
fn process_write(
    accounts: &[AccountInfo],
    pubkey: &Pubkey,
    offset: u32,
    data_chunk: Vec<u8>,
    blockhash: Hash,
    chunks_bump: u8,
    buffer_bump: u8,
) -> ProgramResult {
    msg!("Instruction: Write");

    let [authority_info, chunks_account_info, buffer_account_info] = accounts
    else {
        msg!("Need the following accounts: [authority, chunks, buffer ], but got {}", accounts.len());
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    assert_is_signer(authority_info, "authority")?;

    verify_seeds_and_pdas(
        authority_info,
        chunks_account_info,
        buffer_account_info,
        pubkey,
        &blockhash,
        chunks_bump,
        buffer_bump,
    )?;

    msg!("Updating Buffer and Chunks accounts [ _, chunks_acc_len, buffer_acc_len, offset, size ]");

    {
        let buffer_data = buffer_account_info.data.borrow();
        let chunks_data = chunks_account_info.data.borrow();

        // Interpolating lens and offset increases CUs by ~1200.
        // So we use this less pretty way since it still gives us the info we need
        sol_log_64(
            0,
            chunks_data.len() as u64,
            buffer_data.len() as u64,
            offset as u64,
            data_chunk.len() as u64,
        );

        if offset as usize + data_chunk.len() > buffer_data.len() {
            let err = CommittorError::OffsetChunkOutOfRange(
                data_chunk.len(),
                offset,
                buffer_data.len(),
            );
            msg!("ERR: {}", err);
            return Err(err.into());
        }
    }

    let mut buffer = buffer_account_info.data.borrow_mut();
    buffer[offset as usize..offset as usize + data_chunk.len()]
        .copy_from_slice(&data_chunk);

    let mut chunks_data = chunks_account_info.data.borrow_mut();
    let mut chunks = Chunks::try_from_slice(&chunks_data)?;
    chunks.set_offset(offset as usize)?;
    chunks_data.copy_from_slice(&to_vec(&chunks)?);

    Ok(())
}

// -----------------
// process_close
// -----------------
pub fn process_close(
    accounts: &[AccountInfo],
    pubkey: &Pubkey,
    blockhash: Hash,
    chunks_bump: u8,
    buffer_bump: u8,
) -> ProgramResult {
    msg!("Instruction: Close");

    let [authority_info, chunks_account_info, buffer_account_info] = accounts
    else {
        msg!("Need the following accounts: [authority, chunks, buffer ], but got {}", accounts.len());
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    assert_is_signer(authority_info, "authority")?;

    verify_seeds_and_pdas(
        authority_info,
        chunks_account_info,
        buffer_account_info,
        pubkey,
        &blockhash,
        chunks_bump,
        buffer_bump,
    )?;

    msg!("Closing Chunks and Buffer accounts");
    close_and_refund_authority(authority_info, chunks_account_info)?;
    close_and_refund_authority(authority_info, buffer_account_info)?;

    Ok(())
}

fn verify_seeds_and_pdas(
    authority_info: &AccountInfo,
    chunks_account_info: &AccountInfo,
    buffer_account_info: &AccountInfo,
    pubkey: &Pubkey,
    blockhash: &Hash,
    chunks_bump: u8,
    buffer_bump: u8,
) -> ProgramResult {
    let chunks_bump = &[chunks_bump];
    let (_chunks_seeds, _chunks_pda) = verified_seeds_and_pda!(
        chunks,
        authority_info,
        pubkey,
        chunks_account_info,
        blockhash,
        chunks_bump
    );

    let buffer_bump = &[buffer_bump];
    let (_buffer_seeds, _buffer_pda) = verified_seeds_and_pda!(
        buffer,
        authority_info,
        pubkey,
        buffer_account_info,
        blockhash,
        buffer_bump
    );
    Ok(())
}
