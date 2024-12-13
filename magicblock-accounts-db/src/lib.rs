pub mod account_info;
mod account_locks;
pub mod accounts;
pub mod accounts_cache;
pub mod accounts_db;
pub mod accounts_update_notifier_interface;
pub mod errors;
mod persist;
pub mod verify_accounts_hash_in_background;
pub use persist::{AccountsPersister, FLUSH_ACCOUNTS_SLOT_FREQ};

pub const ACCOUNTS_RUN_DIR: &str = "run";
pub const ACCOUNTS_SNAPSHOT_DIR: &str = "snapshot";

// In order to be 100% compatible with the accounts_db API we export the traits
// from the module it expects them to be in.
use solana_accounts_db::accounts_db::DEFAULT_FILE_SIZE;

// We re-export solana_accounts_db traits until all crates use our replacement
// of the accounts-db
pub mod accounts_file {
    pub use solana_accounts_db::accounts_file::ALIGN_BOUNDARY_OFFSET;
}
pub mod accounts_hash {
    pub use solana_accounts_db::accounts_hash::AccountHash;
}
pub mod accounts_index {
    pub use solana_accounts_db::accounts_index::{
        AccountIndex, AccountSecondaryIndexes, IsCached, ScanConfig,
        ZeroLamport,
    };
}
pub mod append_vec {
    pub use solana_accounts_db::append_vec::{
        aligned_stored_size, STORE_META_OVERHEAD,
    };
}
pub mod account_storage {
    pub use solana_accounts_db::{
        account_storage::*, accounts_db::AccountStorageEntry,
    };
}
pub mod blockhash_queue {
    pub use solana_accounts_db::blockhash_queue::*;
}
pub mod inline_spl_token {
    pub use solana_accounts_db::inline_spl_token::*;
}
pub mod inline_spl_token_2022 {
    pub use solana_accounts_db::inline_spl_token_2022::*;
}
pub mod storable_accounts {
    pub use solana_accounts_db::storable_accounts::StorableAccounts;
}
pub mod transaction_results {
    pub use solana_accounts_db::transaction_results::*;
}
