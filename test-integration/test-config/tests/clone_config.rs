use integration_test_tools::{
    loaded_accounts::LoadedAccounts, validator::cleanup,
};
use magicblock_config::PrepareLookupTables;
use test_config::{
    count_lookup_table_transactions, delegate_and_clone,
    start_validator_with_clone_config, wait_for_startup,
};
use test_tools_core::init_logger;

#[test]
fn test_clone_config_never() {
    init_logger!();

    let (_temp_dir, mut validator, ctx) = start_validator_with_clone_config(
        PrepareLookupTables::Never,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

    wait_for_startup(&mut validator);

    // Create account, delegate, and trigger cloning
    let _payer = delegate_and_clone(&ctx, &mut validator);

    // Verify that no lookup table transactions were created (clone config is Never)
    let lookup_table_tx_count =
        count_lookup_table_transactions(&ctx, &mut validator);
    assert_eq!(lookup_table_tx_count, 0, "Expected no lookup table transactions when clone config is Never, but found {}", lookup_table_tx_count);

    cleanup(&mut validator);
}

#[test]
fn test_clone_config_always() {
    init_logger!();

    let loaded_accounts =
        LoadedAccounts::with_delegation_program_test_authority();
    let (_temp_dir, mut validator, ctx) = start_validator_with_clone_config(
        PrepareLookupTables::Always,
        &loaded_accounts,
    );

    // Wait for validator to start up properly
    wait_for_startup(&mut validator);

    // Create account, delegate, and trigger cloning
    let _payer = delegate_and_clone(&ctx, &mut validator);

    // Verify that lookup table transactions were created (clone config is Always)
    let lookup_table_tx_count =
        count_lookup_table_transactions(&ctx, &mut validator);
    assert_eq!(lookup_table_tx_count, 2, "Expected 2 lookup table transactions when clone config is Always (common keys + account keys), but found {}", lookup_table_tx_count);

    cleanup(&mut validator);
}
