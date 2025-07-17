use integration_test_tools::loaded_accounts::LoadedAccounts;
use magicblock_config::PrepareLookupTables;
use test_config::{cleanup, delegate_and_clone, start_validator_with_clone_config, wait_for_startup};

#[test]
fn test_clone_config_never() {
    let loaded_accounts = LoadedAccounts::with_delegation_program_test_authority();
    let (_temp_dir, mut validator, ctx) = start_validator_with_clone_config(
        PrepareLookupTables::Never,
        &loaded_accounts,
    );
    
    // Wait for validator to start up properly
    wait_for_startup(&mut validator);
    
    // Create account, delegate, and trigger cloning
    let _payer = delegate_and_clone(&ctx, &mut validator);
    
    // TODO: In the next step, we will add assertions to verify
    // that pubkey reservation does not happen when clone config is Never
    
    cleanup(&mut validator);
}

#[test]
fn test_clone_config_always() {
    let loaded_accounts = LoadedAccounts::with_delegation_program_test_authority();
    let (_temp_dir, mut validator, ctx) = start_validator_with_clone_config(
        PrepareLookupTables::Always,
        &loaded_accounts,
    );
    
    // Wait for validator to start up properly
    wait_for_startup(&mut validator);
    
    // Create account, delegate, and trigger cloning
    let _payer = delegate_and_clone(&ctx, &mut validator);
    
    // TODO: In the next step, we will add assertions to verify
    // that pubkey reservation happens when clone config is Always
    
    cleanup(&mut validator);
}
