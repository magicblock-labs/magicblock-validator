use integration_test_tools::loaded_accounts::LoadedAccounts;
use magicblock_config::PrepareLookupTables;
use test_config::{cleanup, start_validator_with_clone_config, wait_for_startup};

#[test]
fn test_clone_config_never() {
    let loaded_accounts = LoadedAccounts::with_delegation_program_test_authority();
    let (_temp_dir, validator) = start_validator_with_clone_config(
        PrepareLookupTables::Never,
        &loaded_accounts,
    );
    
    let Some(mut validator) = validator else {
        panic!("Failed to start validator with clone config Never");
    };
    
    // Wait for validator to start up properly
    wait_for_startup(&mut validator);
    
    // TODO: In the next step, we will add the actual test body to verify
    // that pubkey reservation does not happen when clone config is Never
    
    cleanup(&mut validator);
}

#[test]
fn test_clone_config_always() {
    let loaded_accounts = LoadedAccounts::with_delegation_program_test_authority();
    let (_temp_dir, validator) = start_validator_with_clone_config(
        PrepareLookupTables::Always,
        &loaded_accounts,
    );
    
    let Some(mut validator) = validator else {
        panic!("Failed to start validator with clone config Always");
    };
    
    // Wait for validator to start up properly
    wait_for_startup(&mut validator);
    
    // TODO: In the next step, we will add the actual test body to verify
    // that pubkey reservation happens when clone config is Always
    
    cleanup(&mut validator);
}
