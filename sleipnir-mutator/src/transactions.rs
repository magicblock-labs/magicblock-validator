use sleipnir_program::{
    sleipnir_instruction::{self, AccountModification},
    validator_authority, validator_authority_id,
};
use solana_sdk::{
    account::Account, bpf_loader_upgradeable, clock::Slot, hash::Hash,
    pubkey::Pubkey, transaction::Transaction,
};

use crate::{
    account_modifications::{
        mods_to_clone_account, CloningAccountModifications,
    },
    errors::MutatorResult,
    Cluster,
};

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
    let cloning_account_modifications = mods_to_clone_account(
        cluster,
        account_pubkey,
        account,
        slot,
        overrides,
    )
    .await?;
    match cloning_account_modifications {
        CloningAccountModifications::Account {
            account_modification,
        } => Ok(vec![sleipnir_instruction::modify_accounts(
            vec![account_modification],
            recent_blockhash,
        )]),
        CloningAccountModifications::Program {
            program_modification,
            program_data_modification,
            program_buffer_modification,
            program_idl_modification,
        } => {
            let upgrade_tx = transaction_bpf_loader_upgrade(
                &program_modification.pubkey,
                &program_buffer_modification.pubkey,
                recent_blockhash,
            );
            let mut account_modifications = vec![
                program_modification,
                program_data_modification,
                program_buffer_modification,
            ];
            if let Some(program_idl_modification) = program_idl_modification {
                account_modifications.push(program_idl_modification)
            }
            Ok(vec![
                sleipnir_instruction::modify_accounts(
                    account_modifications,
                    recent_blockhash,
                ),
                upgrade_tx,
            ])
        }
    }
}

fn transaction_bpf_loader_upgrade(
    program_pubkey: &Pubkey,
    program_buffer_pubkey: &Pubkey,
    recent_blockhash: Hash,
) -> Transaction {
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
