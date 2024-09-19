use sleipnir_mutator::errors::MutatorModificationError;
use solana_sdk::{account::Account, pubkey::Pubkey, signature::Signature};
use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum AccountDumperError {
    #[error(transparent)]
    TransactionError(#[from] solana_sdk::transaction::TransactionError),

    #[error(transparent)]
    MutatorModificationError(#[from] MutatorModificationError),
}

pub type AccountDumperResult<T> = Result<T, AccountDumperError>;

// TODO - this could probably be deprecated in favor of:
//  - a TransactionExecutor trait with a service implementation passed as parameter to the AccountCloner
//  - using the mutator's functionality directly inside of the AccountCloner
//  - work tracked here: https://github.com/magicblock-labs/magicblock-validator/issues/159
pub trait AccountDumper {
    // Overrides the account in the bank to make sure it's usable as a potential wallet account
    // in future transactions that account can be used for signing things, and we need to sync its lamports
    fn dump_wallet_account(
        &self,
        pubkey: &Pubkey,
        account: &Account,
        lamports: Option<u64>,
    ) -> AccountDumperResult<Signature>;

    // Overrides the account in the bank to make sure it's a PDA that can be used as readonly
    // Future transactions should be able to read from it (but not write) on the account as-is
    fn dump_undelegated_account(
        &self,
        pubkey: &Pubkey,
        account: &Account,
    ) -> AccountDumperResult<Signature>;

    // Overrides the account in the bank to make sure it's a ready to use delegated account
    // Transactions should be able to write to it, we need to make sure the owner is set correctly
    fn dump_delegated_account(
        &self,
        pubkey: &Pubkey,
        account: &Account,
        owner: &Pubkey,
    ) -> AccountDumperResult<Signature>;

    // Overrides the accounts in the bank to make sure the program is usable normally (and upgraded)
    // We make sure all accounts involved in the program are present in the bank with latest state
    fn dump_program_accounts(
        &self,
        program_id: &Pubkey,
        program_id_account: &Account,
        program_data: &Pubkey,
        program_data_account: &Account,
        program_idl: Option<(Pubkey, Account)>,
    ) -> AccountDumperResult<Vec<Signature>>;
}
