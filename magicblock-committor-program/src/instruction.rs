use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::hash::HASH_BYTES;
use solana_pubkey::Pubkey;

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
        /// The commit id of when account got committed,
        /// needed to properly derive the seeds of the PDAs.
        commit_id: u64,
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
        /// The commit id of when account got committed,
        /// needed to properly derive the seeds of the PDAs.
        commit_id: u64,
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
        /// The commit id of when account got committed,
        /// needed to properly derive the seeds of the PDAs.
        commit_id: u64,
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
    /// Ideally it runs in the same transaction as the 'process' instruction.
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
        /// The commit id of when account got committed,
        /// needed to properly derive the seeds of the PDAs.
        commit_id: u64,
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
        // commit_id: u64,
        8 +
        // chunks_bump: u8,
        1 +
        // buffer_bump: u8,
        1 +
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
        // commit_id: u64,
        8 +
        // buffer_bump: u8,
        1 +
        // invocation_count: u16,
        2 +
        // byte align
        6;

pub const IX_WRITE_SIZE_WITHOUT_CHUNKS: u16 =
    // pubkey: Pubkey,
    32+
        // commit_id: u64,
        8 +
        // chunks_bump: u8,
        1 +
        // buffer_bump: u8,
        1 +
        // offset: u32
        32;

pub const IX_CLOSE_SIZE: u16 =
    // pubkey: Pubkey,
    32 +
        // commit_id: u64,
        8 +
        // chunks_bump: u8,
        1 +
        // buffer_bump: u8,
        1;
