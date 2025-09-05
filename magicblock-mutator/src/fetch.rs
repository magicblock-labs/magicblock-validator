use magicblock_program::instruction::AccountModification;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    account::Account, bpf_loader_upgradeable::get_program_data_address,
    clock::Slot, commitment_config::CommitmentConfig, hash::Hash,
    pubkey::Pubkey, transaction::Transaction,
};

use crate::{
    errors::{MutatorError, MutatorResult},
    idl::fetch_program_idl_modification_from_cluster,
    program::{create_program_modifications, ProgramModifications},
    transactions::{
        transaction_to_clone_program, transaction_to_clone_regular_account,
    },
    Cluster,
};

pub async fn fetch_account_from_cluster(
    cluster: &Cluster,
    pubkey: &Pubkey,
) -> MutatorResult<Account> {
    let rpc_client = RpcClient::new_with_commitment(
        cluster.url().to_string(),
        CommitmentConfig::confirmed(),
    );
    rpc_client
        .get_account(pubkey)
        .await
        .map_err(MutatorError::RpcClientError)
}

/// Downloads an account from the provided cluster and returns a list of transaction that
/// will apply modifications to match the state of the remote chain.
/// If [overrides] are provided the included fields will be changed on the account
/// that was downloaded from the cluster before the modification transaction is
/// created.
pub async fn transaction_to_clone_pubkey_from_cluster(
    cluster: &Cluster,
    needs_upgrade: bool,
    pubkey: &Pubkey,
    recent_blockhash: Hash,
    slot: Slot,
    overrides: Option<AccountModification>,
) -> MutatorResult<Transaction> {
    // Download the account
    let account = &fetch_account_from_cluster(cluster, pubkey).await?;
    // If it's a regular account that's not executable (program), use happy path
    if !account.executable {
        return Ok(transaction_to_clone_regular_account(
            pubkey,
            account,
            overrides,
            recent_blockhash,
        ));
    }
    // To clone a program we need to update multiple accounts at the same time
    let program_id_pubkey = pubkey;
    let program_id_account = account;
    // The program data needs to be cloned, download the executable account
    let program_data_pubkey = get_program_data_address(program_id_pubkey);
    let program_data_account =
        fetch_account_from_cluster(cluster, &program_data_pubkey).await?;
    // Compute the modifications needed to update the program
    let ProgramModifications {
        program_id_modification,
        program_data_modification,
        program_buffer_modification,
    } = create_program_modifications(
        program_id_pubkey,
        program_id_account,
        &program_data_pubkey,
        &program_data_account,
        slot,
    )
    .map_err(MutatorError::MutatorModificationError)?;
    // Try to fetch the IDL if possible
    let program_idl_modification =
        fetch_program_idl_modification_from_cluster(cluster, program_id_pubkey)
            .await;
    // Done, generate the transaction as normal
    Ok(transaction_to_clone_program(
        needs_upgrade,
        program_id_modification,
        program_data_modification,
        program_buffer_modification,
        program_idl_modification,
        recent_blockhash,
    ))
}
