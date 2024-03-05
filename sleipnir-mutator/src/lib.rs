mod account_modification;
mod accounts;
pub mod errors;

pub use account_modification::AccountModification;
pub use accounts::AccountProcessor;
use errors::MutatorResult;
use sleipnir_program::sleipnir_instruction;
use solana_program::hash::Hash;
use solana_sdk::{genesis_config::ClusterType, transaction::Transaction};

#[derive(Clone)]
pub struct Mutator {
    pub accounts_processor: AccountProcessor,
}

impl Mutator {
    pub fn new(development_url: &str) -> Self {
        let accounts_processor = AccountProcessor::new(development_url);
        Self { accounts_processor }
    }

    /// Creates a transaction that will apply the provided account modifications to the
    /// respective accounts.
    pub fn transaction_to_modify_accounts(
        &self,
        modificiations: Vec<AccountModification>,
        recent_blockhash: Hash,
    ) -> MutatorResult<Transaction> {
        let modifications = modificiations
            .into_iter()
            .map(|modification| {
                let (pubkey, modification) =
                    modification.try_into_luzid_program_account_modification()?;
                Ok((pubkey, modification))
            })
            .collect::<MutatorResult<Vec<_>>>()?;

        Ok(luzid_instruction::modify_accounts(
            modifications,
            recent_blockhash,
        ))
    }

    /// Downloads an account from the provided cluster and returns a transaction that
    /// that will apply modifications to the same account in development to match the
    /// state of the remote account.
    pub fn transaction_to_clone_account_from_cluster(
        &self,
        cluster: ClusterType,
        account_address: &str,
        recent_blockhash: Hash,
    ) -> MutatorResult<Transaction> {
        let mods_to_clone = self
            .accounts_processor
            .mods_to_clone_account(cluster, account_address)?;
        self.transaction_to_modify_accounts(mods_to_clone, recent_blockhash)
    }
}
