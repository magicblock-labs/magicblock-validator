use sleipnir_program::{
    sleipnir_instruction::{modify_accounts, AccountModification},
    validator_authority, validator_authority_id,
};
use solana_sdk::{
    account::Account, bpf_loader_upgradeable, clock::Slot, hash::Hash,
    pubkey::Pubkey, transaction::Transaction,
};

use crate::{
    account::resolve_account_modification,
    errors::{MutatorError, MutatorResult},
    program::{resolve_program_modifications, ProgramModifications},
    utils::{fetch_account, get_pubkey_program_data},
    Cluster,
};

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

pub async fn transactions_to_clone_account_from_cluster(
    cluster: &Cluster,
    is_upgrade: bool,
    account_pubkey: &Pubkey,
    account_remote: &Account,
    recent_blockhash: Hash,
    slot: Slot,
    overrides: Option<AccountModification>,
) -> MutatorResult<Vec<Transaction>> {
    // If it's a regular account that's not executable (program), use happy path
    if !account_remote.executable {
        return Ok(vec![transaction_to_clone_regular_account(
            account_pubkey,
            account_remote,
            overrides,
            recent_blockhash,
        )]);
    }
    // To clone a program we need to update multiple accounts at the same time
    let program_pubkey = account_pubkey;
    let program_data_pubkey = get_pubkey_program_data(program_pubkey);

    // The program data needs to be cloned, download the executable account
    let program_data_account = fetch_account(cluster, &program_data_pubkey)
        .await
        .map_err(|err| {
            MutatorError::FailedToCloneProgramExecutableDataAccount(
                *program_pubkey,
                err,
            )
        })?;

    transactions_to_clone_program(
        cluster,
        is_upgrade,
        account_pubkey,
        account_remote,
        slot,
        recent_blockhash,
    )
    .await
}

pub async fn transactions_to_clone_program_from_cluster(
    cluster: &Cluster,
    is_upgrade: bool,
    account_pubkey: &Pubkey,
    account_remote: &Account,
    recent_blockhash: Hash,
    slot: Slot,
    overrides: Option<AccountModification>,
) -> MutatorResult<Vec<Transaction>> {
}

pub fn transaction_to_clone_regular_account(
    account_pubkey: &Pubkey,
    account_remote: &Account,
    overrides: Option<AccountModification>,
    recent_blockhash: Hash,
) -> Transaction {
    // Just a single mutation for regular accounts, just dump the data directly
    let account_modification =
        resolve_account_modification(account_pubkey, account_remote, overrides);
    // We only need a single transaction with a single mutation in this case
    modify_accounts(vec![account_modification], recent_blockhash)
}

pub fn transactions_to_clone_program(
    needs_upgrade: bool,
    program_modification: AccountModification,
    program_data_modification: AccountModification,
    program_buffer_modification: AccountModification,
    program_idl_modification: Option<AccountModification>,
    slot: Slot,
    recent_blockhash: Hash,
) -> MutatorResult<Vec<Transaction>> {
    // We'll need to run the upgrade IX based on those
    let program_pubkey = program_modification.pubkey;
    let program_buffer_pubkey = program_buffer_modification.pubkey;
    // List all necessary account modifications (for the first step)
    let mut account_modifications = vec![
        program_modification,
        program_data_modification,
        program_buffer_modification,
    ];
    if let Some(program_idl_modification) = program_idl_modification {
        account_modifications.push(program_idl_modification)
    }
    // If the program does not exist yet, we just need to update it's data and don't
    // need to explicitly update using the BPF loader's Upgrade IX
    if !needs_upgrade {
        return Ok(vec![modify_accounts(
            account_modifications,
            recent_blockhash,
        )]);
    }
    // If it's an upgrade of the program rather than the first deployment,
    // generate a modify TX and an Upgrade TX following it
    Ok(vec![
        // First dump the necessary set of account to our bank/ledger
        modify_accounts(account_modifications, recent_blockhash),
        // Then we run the official BPF upgrade IX to notify the system of the new program
        transaction_to_run_bpf_loader_upgrade(
            &program_pubkey,
            &program_buffer_pubkey,
            recent_blockhash,
        ),
    ])
}

fn transaction_to_run_bpf_loader_upgrade(
    program_pubkey: &Pubkey,
    program_buffer_pubkey: &Pubkey,
    recent_blockhash: Hash,
) -> Transaction {
    // The validator is marked as the upgrade authority of all program accounts
    let validator_keypair = &validator_authority();
    let validator_pubkey = &validator_authority_id();
    let ix = bpf_loader_upgradeable::upgrade(
        program_pubkey,
        program_buffer_pubkey,
        validator_pubkey,
        validator_pubkey,
    );
    Transaction::new_signed_with_payer(
        &[ix],
        Some(validator_pubkey),
        &[validator_keypair],
        recent_blockhash,
    )
}
