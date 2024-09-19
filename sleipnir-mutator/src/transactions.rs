use sleipnir_program::{
    sleipnir_instruction::{
        modify_accounts, modify_accounts_instruction, AccountModification,
    },
    validator_authority, validator_authority_id,
};
use solana_sdk::{
    account::Account, bpf_loader_upgradeable, hash::Hash, pubkey::Pubkey,
    transaction::Transaction,
};

pub fn transaction_to_clone_regular_account(
    pubkey: &Pubkey,
    account: &Account,
    overrides: Option<AccountModification>,
    recent_blockhash: Hash,
) -> Transaction {
    // Just a single mutation for regular accounts, just dump the data directly, while applying overrides
    let mut account_modification = AccountModification::from((pubkey, account));
    if let Some(overrides) = overrides {
        if let Some(lamports) = overrides.lamports {
            account_modification.lamports = Some(lamports);
        }
        if let Some(owner) = &overrides.owner {
            account_modification.owner = Some(*owner);
        }
        if let Some(executable) = overrides.executable {
            account_modification.executable = Some(executable);
        }
        if let Some(data) = &overrides.data {
            account_modification.data = Some(data.clone());
        }
        if let Some(rent_epoch) = overrides.rent_epoch {
            account_modification.rent_epoch = Some(rent_epoch);
        }
    }
    // We only need a single transaction with a single mutation in this case
    modify_accounts(vec![account_modification], recent_blockhash)
}

pub fn transaction_to_clone_program(
    needs_upgrade: bool,
    program_id_modification: AccountModification,
    program_data_modification: AccountModification,
    program_buffer_modification: AccountModification,
    program_idl_modification: Option<AccountModification>,
    recent_blockhash: Hash,
) -> Transaction {
    // We'll need to run the upgrade IX based on those
    let program_id_pubkey = program_id_modification.pubkey;
    let program_buffer_pubkey = program_buffer_modification.pubkey;
    // List all necessary account modifications (for the first step)
    let mut account_modifications = vec![
        program_id_modification,
        program_data_modification,
        program_buffer_modification,
    ];
    if let Some(program_idl_modification) = program_idl_modification {
        account_modifications.push(program_idl_modification)
    }
    // If the program does not exist yet, we just need to update it's data and don't
    // need to explicitly update using the BPF loader's Upgrade IX
    if !needs_upgrade {
        return modify_accounts(account_modifications, recent_blockhash);
    }
    // First dump the necessary set of account to our bank/ledger
    let modify_ix = modify_accounts_instruction(account_modifications);
    // The validator is marked as the upgrade authority of all program accounts
    let validator_pubkey = &validator_authority_id();
    let upgrade_ix = bpf_loader_upgradeable::upgrade(
        &program_id_pubkey,
        &program_buffer_pubkey,
        validator_pubkey,
        validator_pubkey,
    );
    // Sign the transaction
    Transaction::new_signed_with_payer(
        &[modify_ix, upgrade_ix],
        Some(validator_pubkey),
        &[&validator_authority()],
        recent_blockhash,
    )
}
