use sleipnir_program::sleipnir_instruction::{self, AccountModification};
use solana_sdk::{
    account::Account, clock::Slot, hash::Hash, pubkey::Pubkey,
    transaction::Transaction,
};

use crate::{accounts::mods_to_clone_account, errors::MutatorResult, Cluster};

/// Downloads an account from the provided cluster and returns a transaction that
/// that will apply modifications to the same account in development to match the
/// state of the remote account.
/// If [overrides] are provided the included fields will be changed on the account
/// that was downloaded from the cluster before the modification transaction is
/// created.
pub async fn transactions_to_clone_account_from_cluster(
    cluster: &Cluster,
    account_pubkey: &Pubkey,
    account: Option<Account>,
    recent_blockhash: Hash,
    slot: Slot,
    overrides: Option<AccountModification>,
) -> MutatorResult<Vec<Transaction>> {
    let account_modifications = mods_to_clone_account(
        cluster,
        account_pubkey,
        account,
        slot,
        overrides,
    )
    .await?;
    Ok(vec![sleipnir_instruction::modify_accounts(
        account_modifications,
        recent_blockhash,
    )])
}
