use dlp::pda::commit_state_pda_from_delegated_account;
use magicblock_committor_program::CommitableAccount;
use solana_pubkey::Pubkey;
use solana_sdk::{clock::Slot, hash::Hash};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitInfo {
    /// A commit for an account that has no data. In this case we are trying to
    /// commit changes to its lamports.
    EmptyAccount {
        /// The on chain address of the delegated account
        pubkey: Pubkey,
        /// The original owner of the delegated account on chain
        delegated_account_owner: Pubkey,
        /// The ephemeral slot at which those changes were requested
        slot: Slot,
        /// The ephemeral blockhash at which those changes were requested
        ephemeral_blockhash: Hash,
        /// If we also undelegate the account after committing it
        undelegate: bool,
        /// Lamports of the account in the ephemeral
        lamports: u64,
        /// This id will be the same for accounts whose commits need to
        /// be applied atomically in a single transaction
        /// For single account commits it is also set for consistency
        bundle_id: u64,
        /// If `true` the account commit is finalized after it was processed
        finalize: bool,
    },
    /// A commit for an account that is part of a bundle whose data is small enough
    /// to fit into a single process commit instruction.
    DataAccount {
        /// The on chain address of the delegated account
        pubkey: Pubkey,
        /// The account where the delegated account state is committed and stored
        /// until it is finalized
        commit_state: Pubkey,
        /// The original owner of the delegated account on chain
        delegated_account_owner: Pubkey,
        /// The ephemeral slot at which those changes were requested
        slot: Slot,
        /// The ephemeral blockhash at which those changes were requested
        ephemeral_blockhash: Hash,
        /// If we also undelegate the account after committing it
        undelegate: bool,
        /// Lamports of the account in the ephemeral
        lamports: u64,
        /// This id will be the same for accounts whose commits need to
        /// be applied atomically in a single transaction
        /// For single account commits it is also set for consistency
        bundle_id: u64,
        /// If `true` the account commit is finalized after it was processed
        finalize: bool,
    },
    /// A commit for an account that is part of a bundle whose total data is so large
    /// that we send the data in chunks to a buffer account before processing the
    /// commit.
    BufferedDataAccount {
        /// The on chain address of the delegated account
        pubkey: Pubkey,
        /// The account where the delegated account state is committed and stored
        /// until it is finalized
        commit_state: Pubkey,
        /// The original owner of the delegated account on chain
        delegated_account_owner: Pubkey,
        /// The ephemeral slot at which those changes were requested
        slot: Slot,
        /// The ephemeral blockhash at which those changes were requested
        ephemeral_blockhash: Hash,
        /// If we also undelegate the account after committing it
        undelegate: bool,
        /// The account that tracked that all chunks got written to the [CommitInfo::buffer_pda]
        chunks_pda: Pubkey,
        /// The temporary address where the data of the account is stored
        buffer_pda: Pubkey,
        /// Lamports of the account in the ephemeral
        lamports: u64,
        /// This id will be the same for accounts whose commits need to
        /// be applied atomically in a single transaction
        /// For single account commits it is also set for consistency
        bundle_id: u64,
        /// If `true` the account commit is finalized after it was processed
        finalize: bool,
    },
}

impl CommitInfo {
    pub fn from_small_data_account(
        commitable: CommitableAccount,
        ephemeral_blockhash: Hash,
        finalize: bool,
    ) -> Self {
        Self::DataAccount {
            pubkey: commitable.pubkey,
            delegated_account_owner: commitable.delegated_account_owner,
            slot: commitable.slot,
            ephemeral_blockhash,
            undelegate: commitable.undelegate,
            lamports: commitable.lamports,
            bundle_id: commitable.bundle_id,
            finalize,
            commit_state: commit_state_pda_from_delegated_account(
                &commitable.pubkey,
            ),
        }
    }

    pub fn pubkey(&self) -> Pubkey {
        match self {
            Self::EmptyAccount { pubkey, .. } => *pubkey,
            Self::DataAccount { pubkey, .. } => *pubkey,
            Self::BufferedDataAccount { pubkey, .. } => *pubkey,
        }
    }

    pub fn commit_state(&self) -> Option<Pubkey> {
        match self {
            Self::BufferedDataAccount { commit_state, .. } => {
                Some(*commit_state)
            }
            Self::DataAccount { commit_state, .. } => Some(*commit_state),
            _ => None,
        }
    }

    pub fn lamports(&self) -> u64 {
        match self {
            Self::EmptyAccount { lamports, .. } => *lamports,
            Self::DataAccount { lamports, .. } => *lamports,
            Self::BufferedDataAccount { lamports, .. } => *lamports,
        }
    }

    pub fn bundle_id(&self) -> u64 {
        match self {
            Self::EmptyAccount { bundle_id, .. } => *bundle_id,
            Self::DataAccount { bundle_id, .. } => *bundle_id,
            Self::BufferedDataAccount { bundle_id, .. } => *bundle_id,
        }
    }

    pub fn undelegate(&self) -> bool {
        match self {
            Self::EmptyAccount { undelegate, .. } => *undelegate,
            Self::DataAccount { undelegate, .. } => *undelegate,
            Self::BufferedDataAccount { undelegate, .. } => *undelegate,
        }
    }

    pub fn chunks_pda(&self) -> Option<Pubkey> {
        match self {
            Self::BufferedDataAccount { chunks_pda, .. } => Some(*chunks_pda),
            _ => None,
        }
    }

    pub fn buffer_pda(&self) -> Option<Pubkey> {
        match self {
            Self::BufferedDataAccount { buffer_pda, .. } => Some(*buffer_pda),
            _ => None,
        }
    }

    pub fn pdas(&self) -> Option<(Pubkey, Pubkey)> {
        match self {
            Self::BufferedDataAccount {
                chunks_pda,
                buffer_pda,
                ..
            } => Some((*chunks_pda, *buffer_pda)),
            _ => None,
        }
    }
}
