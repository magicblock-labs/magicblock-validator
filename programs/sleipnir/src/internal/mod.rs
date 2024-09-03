mod accounts_removal;
pub(crate) use accounts_removal::process_remove_accounts_pending_removal;
pub use accounts_removal::{
    process_accounts_pending_removal_transaction, ValidatorAccountsRemover,
};
