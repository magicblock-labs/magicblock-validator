use solana_pubkey::{pubkey, Pubkey};

pub mod auth;
pub mod quote;
pub mod service;
pub mod transaction;
pub mod types;

pub use service::{
    filter_account, filter_accounts, filter_keyed_accounts,
    filter_program_accounts, filter_signatures, permission_for_account,
    permissions_for_accounts, visible_transaction_accounts,
    QueryFilteringError, QueryFilteringService,
};
pub use transaction::{
    check_transaction_admission, filter_confirmed_transaction,
    WHITELISTED_PROGRAMS,
};

pub const PERMISSION_PROGRAM_ID: Pubkey =
    pubkey!("ACLseoPoyC3cBqoUtkbjZ4aDrkurZW86v19pXz2XQnp1");
