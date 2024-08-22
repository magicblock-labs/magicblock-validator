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
    errors::MutatorResult,
    program::{resolve_program_modifications, ProgramModifications},
    utils::fetch_account,
    Cluster,
};

/// Downloads an account from the provided cluster and returns a list of transaction that
/// that will apply modifications to match the state of the remote chain.
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
    // Download the account if not already
    let account = match account {
        Some(account) => account,
        None => fetch_account(cluster, account_pubkey).await?,
    };
    // If it's a regular account that's not executable (program), use happy path
    if !account.executable {
        return Ok(vec![transaction_to_clone_regular_account(
            account_pubkey,
            &account,
            overrides,
            recent_blockhash,
        )]);
    }
    // If it's a program we'll return the list of necessary transactions
    transactions_to_clone_program(
        cluster,
        account_pubkey,
        &account,
        slot,
        recent_blockhash,
    )
    .await
}

fn transaction_to_clone_regular_account(
    account_pubkey: &Pubkey,
    account: &Account,
    overrides: Option<AccountModification>,
    recent_blockhash: Hash,
) -> Transaction {
    // Just a single mutation for regular accounts, just dump the data directly
    let account_modification =
        resolve_account_modification(account_pubkey, account, overrides);
    modify_accounts(vec![account_modification], recent_blockhash)
}

async fn transactions_to_clone_program(
    cluster: &Cluster,
    account_pubkey: &Pubkey,
    account: &Account,
    slot: Slot,
    recent_blockhash: Hash,
) -> MutatorResult<Vec<Transaction>> {
    // To clone a program we need to update multiple accounts at the same time
    let ProgramModifications {
        program_modification,
        program_data_modification,
        program_buffer_modification,
        program_idl_modification,
    } = resolve_program_modifications(cluster, account_pubkey, account, slot)
        .await?;
    let program_pubkey = program_modification.pubkey;
    let program_buffer_pubkey = program_buffer_modification.pubkey;
    // List all necessary modifications
    let mut account_modifications = vec![
        program_modification,
        program_data_modification,
        program_buffer_modification,
    ];
    if let Some(program_idl_modification) = program_idl_modification {
        account_modifications.push(program_idl_modification)
    }
    // Generate the list of transactions to be run in order
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
