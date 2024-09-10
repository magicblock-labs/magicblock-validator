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

pub trait AccountDumper {
    fn dump_new_account(
        &self,
        pubkey: &Pubkey,
    ) -> AccountDumperResult<Signature>;

    fn dump_system_account(
        &self,
        pubkey: &Pubkey,
        account: &Account,
        lamports: Option<u64>,
    ) -> AccountDumperResult<Signature>;

    fn dump_regular_account(
        &self,
        pubkey: &Pubkey,
        account: &Account,
    ) -> AccountDumperResult<Signature>;

    fn dump_delegated_account(
        &self,
        pubkey: &Pubkey,
        account: &Account,
        owner: &Pubkey,
    ) -> AccountDumperResult<Signature>;

    fn dump_program_accounts(
        &self,
        program_id: &Pubkey,
        program_id_account: &Account,
        program_data: &Pubkey,
        program_data_account: &Account,
        program_idl: Option<(Pubkey, Account)>,
    ) -> AccountDumperResult<Vec<Signature>>;
}
