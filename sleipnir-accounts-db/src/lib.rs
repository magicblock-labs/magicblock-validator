// TODO(thlorenz): @@@ once done clean this
#![allow(unused)]

pub mod account_info;
mod account_locks;
pub mod accounts;
pub mod accounts_cache;
pub mod accounts_db;
pub mod accounts_update_notifier_interface;
pub mod errors;

// mod traits;
// pub use traits::*;

// In order to be 100% compatible with the accounts_db API we export the traits
// from the module it expects them to be in.

// We re-export solana_accounts_db traits until all crates use our replacement
// of the accounts-db
pub mod accounts_index {
    pub use solana_accounts_db::accounts_index::{IsCached, ZeroLamport};
}
pub mod storable_accounts {
    pub use solana_accounts_db::storable_accounts::StorableAccounts;
}
pub mod account_storage {
    pub use solana_accounts_db::account_storage::*;
}
pub mod transaction_results {
    pub use solana_accounts_db::transaction_results::*;
}
