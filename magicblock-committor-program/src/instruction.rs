use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::hash::Hash;
use solana_program::hash::HASH_BYTES;
use solana_program::instruction::{AccountMeta, Instruction};
use solana_program::system_program;
use solana_pubkey::Pubkey;

use crate::{consts, pdas};

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub enum CommittorInstruction {
    /// Initializes the buffer and [Chunks] accounts which will be used to
    /// [CommittorInstruction::Write] and then [CommittorInstruction::Commit].
    ///
    /// Accounts:
    /// 0. `[signer]`   The validator authority.
    /// 1. `[writable]` The PDA holding the [Chunks] data which track the
    ///    committed chunks.
    /// 2. `[writable]` The PDA buffer account into which we accumulate the data to commit.
    /// 3. `[]`         The system program to facilitate creation of accounts
    Init {
        /// The on chain address of the account we are committing
        /// This is part of the seeds used to derive the buffer and chunk account PDAs.
        pubkey: Pubkey,
        /// The size that the buffer account needs to have in order to track commits
        chunks_account_size: u64,
        /// The size that the buffer account needs to have in order to hold all commits
        buffer_account_size: u64,
        /// The ephemeral blockhash of the changeset we are writing,
        /// needed to properly derive the seeds of the PDAs.
        blockhash: Hash,
        /// The bump to use when deriving seeds and PDA for the [Chunks] account.
        chunks_bump: u8,
        /// The bump to use when deriving seeds and PDA for the buffer account.
        buffer_bump: u8,
        /// The number of chunks that the [Chunks] account will track.
        chunk_count: usize,
        /// The size of each chunk that the [Chunks] account will track.
        chunk_size: u16,
    },
    /// Accounts:
    /// 0. `[signer]`   The validator authority.
    /// 1. `[writable]` The PDA buffer account into which we accumulate the data to commit.
    ReallocBuffer {
        /// The on chain address of the account we are committing
        /// This is part of the seeds used to derive the buffer and chunk account PDAs.
        pubkey: Pubkey,
        /// The size that the buffer account needs to have in order to hold all commits
        buffer_account_size: u64,
        /// The ephemeral blockhash of the changeset we are writing,
        /// needed to properly derive the seeds of the PDAs.
        blockhash: Hash,
        /// The bump to use when deriving seeds and PDA for the buffer account.
        buffer_bump: u8,
        /// The count of invocations of realloc buffer that this instruction represents.
        invocation_count: u16,
    },
    /// Writes a chunk of data into the buffer account and updates the [Chunks] to
    /// show that the chunk has been written.
    ///
    /// Accounts:
    /// 0. `[signer]`   The validator authority.
    /// 1. `[writable]` The PDA holding the [Chunks] data which track the
    ///    committed chunks.
    /// 2. `[writable]` The PDA buffer account into which we accumulate the data to commit.
    Write {
        /// The on chain address of the account we are committing
        /// This is part of the seeds used to derive the buffer and chunk account PDAs.
        pubkey: Pubkey,
        /// The ephemeral blockhash of the changeset we are writing,
        /// needed to properly derive the seeds of the PDAs.
        blockhash: Hash,
        /// The bump to use when deriving seeds and PDA for the [Chunks] account.
        chunks_bump: u8,
        /// The bump to use when deriving seeds and PDA for the buffer account.
        buffer_bump: u8,
        /// Offset in the buffer account where to write the data.
        offset: u32,
        /// The data to write into the buffer account.
        data_chunk: Vec<u8>,
    },
    /// This instruction closes the buffer account and the [Chunks] account.
    ///
    /// It is called by the validator after the instruction that processes the
    /// change set stored in the buffer account and applies the commits to the
    /// relevant accounts.
    /// Ideally it runs in the same transaction as the 'processs' instruction.
    ///
    /// The lamports gained due to closing both accounts are transferred to the
    /// validator authority.
    ///
    /// Accounts:
    /// 0. `[signer]`   The validator authority.
    /// 1. `[writable]` The PDA holding the [Chunks] data which tracked the
    ///    committed chunks and we are now closing.
    /// 2. `[writable]` The PDA buffer account we are closing.
    Close {
        /// The on chain address of the account we committed.
        /// This is part of the seeds used to derive the buffer and chunk account PDAs.
        pubkey: Pubkey,
        /// The ephemeral blockhash of the changeset we are writing,
        /// needed to properly derive the seeds of the PDAs.
        blockhash: Hash,
        /// The bump to use when deriving seeds and PDA for the [Chunks] account.
        chunks_bump: u8,
        /// The bump to use when deriving seeds and PDA for the buffer account.
        buffer_bump: u8,
    },
}

pub const IX_INIT_SIZE: u16 =
    // pubkey: Pubkey,
    32 +
    // chunks_account_size: u64,
    8 +
    // buffer_account_size: u64,
    8 +
    // blockhash: Hash,
    HASH_BYTES as u16 +
    // chunks_bump: u8,
    8 +
    // buffer_bump: u8,
    8 +
    // chunk_count: usize,
    8 +
    // chunk_size: u16,
    2 +
    // byte align
    6;

pub const IX_REALLOC_SIZE: u16 =
    // pubkey: Pubkey,
    32 +
    // buffer_account_size: u64,
    8 +
    // blockhash: Hash,
    HASH_BYTES as u16 +
    // buffer_bump: u8,
    8 +
    // invocation_count: u16,
    2 +
    // byte align
    6;

pub const IX_WRITE_SIZE_WITHOUT_CHUNKS: u16 =
    // pubkey: Pubkey,
    32+
    // blockhash: Hash,
    HASH_BYTES as u16 +
    // chunks_bump: u8,
    8 +
    // buffer_bump: u8,
    8 +
    // offset: u32
    32;

pub const IX_CLOSE_SIZE: u16 =
    // pubkey: Pubkey,
    32 +
    // blockhash: Hash,
    HASH_BYTES as u16 +
    // chunks_bump: u8,
    8 +
    // buffer_bump: u8,
    8;

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
    /// The latest on chain blockhash
    pub blockhash: Hash,
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
        blockhash,
        chunk_count,
        chunk_size,
    } = args;

    let (chunks_pda, chunks_bump) =
        pdas::chunks_pda(&authority, &pubkey, &blockhash);
    let (buffer_pda, buffer_bump) =
        pdas::buffer_pda(&authority, &pubkey, &blockhash);
    let program_id = crate::id();
    let ix = CommittorInstruction::Init {
        pubkey,
        blockhash,
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

// -----------------
// create_realloc_buffer_ix
// -----------------
#[derive(Clone)]
pub struct CreateReallocBufferIxArgs {
    pub authority: Pubkey,
    pub pubkey: Pubkey,
    pub buffer_account_size: u64,
    pub blockhash: Hash,
}

/// Creates the realloc ixs we need to invoke in order to realloc
/// the account to the desired size since we only can realloc up to
/// [consts::MAX_ACOUNT_ALLOC_PER_INSTRUCTION_SIZE] in a single instruction.
/// Returns a tuple with the instructions and a bool indicating if we need to split
/// them into multiple instructions in order to avoid
/// [solana_program::program_error::MAX_INSTRUCTION_TRACE_LENGTH_EXCEEDED]J
pub fn create_realloc_buffer_ixs(
    args: CreateReallocBufferIxArgs,
) -> Vec<Instruction> {
    // We already allocated once during Init and only need to realloc
    // if the buffer is larger than [consts::MAX_ACOUNT_ALLOC_PER_INSTRUCTION_SIZE]
    if args.buffer_account_size
        <= consts::MAX_ACOUNT_ALLOC_PER_INSTRUCTION_SIZE as u64
    {
        return vec![];
    }

    let remaining_size = args.buffer_account_size as i128
        - consts::MAX_ACOUNT_ALLOC_PER_INSTRUCTION_SIZE as i128;

    // A) We just need to realloc once
    if remaining_size <= consts::MAX_ACOUNT_ALLOC_PER_INSTRUCTION_SIZE as i128 {
        return vec![create_realloc_buffer_ix(args, 1)];
    }

    // B) We need to realloc multiple times
    // SAFETY; remaining size > consts::MAX_ACOUNT_ALLOC_PER_INSTRUCTION_SIZE
    create_realloc_buffer_ixs_to_add_remaining(&args, remaining_size as u64)
}

pub fn create_realloc_buffer_ixs_to_add_remaining(
    args: &CreateReallocBufferIxArgs,
    remaining_size: u64,
) -> Vec<Instruction> {
    let invocation_count = (remaining_size as f64
        / consts::MAX_ACOUNT_ALLOC_PER_INSTRUCTION_SIZE as f64)
        .ceil() as u16;

    let mut ixs = vec![];
    for i in 0..invocation_count {
        ixs.push(create_realloc_buffer_ix(args.clone(), i + 1));
    }

    ixs
}

fn create_realloc_buffer_ix(
    args: CreateReallocBufferIxArgs,
    invocation_count: u16,
) -> Instruction {
    let CreateReallocBufferIxArgs {
        authority,
        pubkey,
        buffer_account_size,
        blockhash,
    } = args;
    let (buffer_pda, buffer_bump) =
        pdas::buffer_pda(&authority, &pubkey, &blockhash);

    let program_id = crate::id();
    let ix = CommittorInstruction::ReallocBuffer {
        pubkey,
        buffer_account_size,
        blockhash,
        buffer_bump,
        invocation_count,
    };
    let accounts = vec![
        AccountMeta::new(authority, true),
        AccountMeta::new(buffer_pda, false),
    ];
    Instruction::new_with_borsh(program_id, &ix, accounts)
}

// -----------------
// create_write_ix
// -----------------
pub struct CreateWriteIxArgs {
    pub authority: Pubkey,
    pub pubkey: Pubkey,
    pub offset: u32,
    pub data_chunk: Vec<u8>,
    pub blockhash: Hash,
}

pub fn create_write_ix(args: CreateWriteIxArgs) -> Instruction {
    let CreateWriteIxArgs {
        authority,
        pubkey,
        offset,
        data_chunk,
        blockhash,
    } = args;
    let (chunks_pda, chunks_bump) =
        pdas::chunks_pda(&authority, &pubkey, &blockhash);
    let (buffer_pda, buffer_bump) =
        pdas::buffer_pda(&authority, &pubkey, &blockhash);

    let program_id = crate::id();
    let ix = CommittorInstruction::Write {
        pubkey,
        blockhash,
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
    Instruction::new_with_borsh(program_id, &ix, accounts)
}

// -----------------
// create_close_ix
// -----------------
pub struct CreateCloseIxArgs {
    pub authority: Pubkey,
    pub pubkey: Pubkey,
    pub blockhash: Hash,
}

pub fn create_close_ix(args: CreateCloseIxArgs) -> Instruction {
    let CreateCloseIxArgs {
        authority,
        pubkey,
        blockhash,
    } = args;
    let (chunks_pda, chunks_bump) =
        pdas::chunks_pda(&authority, &pubkey, &blockhash);
    let (buffer_pda, buffer_bump) =
        pdas::buffer_pda(&authority, &pubkey, &blockhash);

    let program_id = crate::id();
    let ix = CommittorInstruction::Close {
        pubkey,
        blockhash,
        chunks_bump,
        buffer_bump,
    };
    let accounts = vec![
        AccountMeta::new(authority, true),
        AccountMeta::new(chunks_pda, false),
        AccountMeta::new(buffer_pda, false),
    ];
    Instruction::new_with_borsh(program_id, &ix, accounts)
}
