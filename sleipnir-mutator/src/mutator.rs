use sleipnir_program::sleipnir_instruction;
use solana_sdk::{clock::Slot, hash::Hash, transaction::Transaction};

use crate::{
    account_modification::AccountModification, accounts::mods_to_clone_account,
    errors::MutatorResult, Cluster,
};

/// Creates a transaction that will apply the provided account modifications to the
/// respective accounts.
pub fn transaction_to_modify_accounts(
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
/// If [overrides] are provided the included fields will be changed on the account
/// that was downloaded from the cluster before the modification transaction is
/// created.
pub async fn transaction_to_clone_account_from_cluster(
    cluster: &Cluster,
    account_address: &str,
    recent_blockhash: Hash,
    slot: Slot,
    overrides: Option<AccountModification>,
) -> MutatorResult<Transaction> {
    let mods_to_clone =
        mods_to_clone_account(cluster, account_address, slot, overrides)
            .await?;
    transaction_to_modify_accounts(mods_to_clone, recent_blockhash)
}
