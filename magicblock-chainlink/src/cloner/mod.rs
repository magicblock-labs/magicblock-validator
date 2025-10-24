use async_trait::async_trait;
use errors::ClonerResult;
use solana_account::AccountSharedData;
use solana_pubkey::Pubkey;
use solana_sdk::signature::Signature;

use crate::remote_account_provider::program_account::LoadedProgram;

pub mod errors;

#[async_trait]
pub trait Cloner: Send + Sync + 'static {
    /// Overrides the account in the bank to make sure it's a PDA that can be used as readonly
    /// Future transactions should be able to read from it (but not write) on the account as-is
    /// NOTE: this will run inside a separate task as to not block account sub handling.
    /// However it includes a channel callback in order to signal once the account was cloned
    /// successfully.
    async fn clone_account(
        &self,
        pubkey: Pubkey,
        account: AccountSharedData,
    ) -> ClonerResult<Signature>;

    // Overrides the accounts in the bank to make sure the program is usable normally (and upgraded)
    // We make sure all accounts involved in the program are present in the bank with latest state
    async fn clone_program(
        &self,
        program: LoadedProgram,
    ) -> ClonerResult<Signature>;
}
