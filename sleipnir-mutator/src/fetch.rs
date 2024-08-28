use sleipnir_program::sleipnir_instruction::AccountModification;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    account::Account, clock::Slot, commitment_config::CommitmentConfig,
    pubkey::Pubkey,
};

use crate::Cluster;

// TODO(vbrunet)
//  - Long term this should probably use the validator's AccountFetcher
//  - Tracked here: https://github.com/magicblock-labs/magicblock-validator/issues/136
pub async fn fetch_account(
    cluster: &Cluster,
    pubkey: &Pubkey,
) -> Result<Account, solana_rpc_client_api::client_error::Error> {
    let rpc_client = RpcClient::new_with_commitment(
        cluster.url().to_string(),
        CommitmentConfig::confirmed(),
    );
    rpc_client.get_account(pubkey).await
}

/// Downloads an account from the provided cluster and returns a list of transaction that
/// will apply modifications to match the state of the remote chain.
/// If [overrides] are provided the included fields will be changed on the account
/// that was downloaded from the cluster before the modification transaction is
/// created.
pub async fn transactions_to_clone_pubkey_from_cluster(
    cluster: &Cluster,
    is_upgrade: bool,
    account_pubkey: &Pubkey,
    recent_blockhash: Hash,
    slot: Slot,
    overrides: Option<AccountModification>,
) -> MutatorResult<Vec<Transaction>> {
    // Download the account
    let account_remote = fetch_account(cluster, account_pubkey).await?;
    // Run the normal procedure
    transactions_to_clone_account_from_cluster(
        cluster,
        is_upgrade,
        account_pubkey,
        &account_remote,
        recent_blockhash,
        slot,
        overrides,
    )
    .await
}
