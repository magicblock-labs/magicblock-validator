use sleipnir_program::sleipnir_instruction;
use solana_sdk::{
    clock::Slot, genesis_config::ClusterType, hash::Hash,
    transaction::Transaction,
};

use crate::{
    account_modification::AccountModification, accounts::AccountProcessor,
    errors::MutatorResult,
};

#[derive(Clone)]
pub struct Mutator {
    pub accounts_processor: AccountProcessor,
}

impl Mutator {
    pub fn new() -> Self {
        let accounts_processor = AccountProcessor::new();
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
                let (pubkey, modification) = modification
                    .try_into_sleipnir_program_account_modification()?;
                Ok((pubkey, modification))
            })
            .collect::<MutatorResult<Vec<_>>>()?;

        Ok(sleipnir_instruction::modify_accounts(
            modifications,
            recent_blockhash,
        ))
    }

    /// Downloads an account from the provided cluster and returns a transaction that
    /// that will apply modifications to the same account in development to match the
    /// state of the remote account.
    pub async fn transaction_to_clone_account_from_cluster(
        &self,
        cluster: ClusterType,
        account_address: &str,
        recent_blockhash: Hash,
        slot: Slot,
    ) -> MutatorResult<Transaction> {
        let mods_to_clone = self
            .accounts_processor
            .mods_to_clone_account(cluster, account_address, slot)
            .await?;
        self.transaction_to_modify_accounts(mods_to_clone, recent_blockhash)
    }
}

impl Default for Mutator {
    fn default() -> Self {
        Self::new()
    }
}
