use integration_test_tools::{
    expect, loaded_accounts::LoadedAccounts, validator::cleanup,
    IntegrationTestContext,
};
use log::*;
use magicblock_config::PrepareLookupTables;
use test_config::{
    count_lookup_table_transactions_on_chain, delegate_and_clone,
    start_validator_with_clone_config, wait_for_startup,
};
use test_kit::init_logger;

fn lookup_table_interaction(
    config: PrepareLookupTables,
) -> (usize, usize, usize) {
    let lookup_table_tx_count_before =
        count_lookup_table_transactions_on_chain(
            &IntegrationTestContext::try_new_chain_only().unwrap(),
        )
        .unwrap();

    let (_temp_dir, mut validator, ctx) = start_validator_with_clone_config(
        config,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );
    wait_for_startup(&mut validator);

    let lookup_table_tx_count_after_start = expect!(
        count_lookup_table_transactions_on_chain(&ctx),
        "Failed to count lookup table transactions on chain",
        validator
    );

    let _payer = delegate_and_clone(&ctx, &mut validator);

    std::thread::sleep(std::time::Duration::from_millis(500));

    let lookup_table_tx_count_after_clone = expect!(
        count_lookup_table_transactions_on_chain(&ctx),
        "Failed to count lookup table transactions on chain",
        validator
    );

    cleanup(&mut validator);

    (
        lookup_table_tx_count_before,
        lookup_table_tx_count_after_start,
        lookup_table_tx_count_after_clone,
    )
}

#[test]
fn test_clone_config_never() {
    init_logger!();

    let (
        lookup_table_tx_count_before,
        lookup_table_tx_count_after_start,
        lookup_table_tx_count_after_clone,
    ) = lookup_table_interaction(PrepareLookupTables::Never);

    debug!(
        "Lookup table tx count before: {}, after start: {}, after clone: {}",
        lookup_table_tx_count_before,
        lookup_table_tx_count_after_start,
        lookup_table_tx_count_after_clone
    );

    // Common pubkeys should be not reserved during validator startup
    assert_eq!(
        lookup_table_tx_count_after_start,
        lookup_table_tx_count_before
    );
    // The pubkeys needed to commit the cloned account should not be reserved when it was cloned
    assert_eq!(
        lookup_table_tx_count_after_clone,
        lookup_table_tx_count_before
    );
}

#[test]
fn test_clone_config_always() {
    init_logger!();

    let (
        lookup_table_tx_count_before,
        lookup_table_tx_count_after_start,
        lookup_table_tx_count_after_clone,
    ) = lookup_table_interaction(PrepareLookupTables::Always);

    debug!(
        "Lookup table tx count before: {}, after start: {}, after clone: {}",
        lookup_table_tx_count_before,
        lookup_table_tx_count_after_start,
        lookup_table_tx_count_after_clone
    );

    // Common pubkeys should be reserved during validator startup, in a single lookup table
    // transaction
    assert_eq!(
        lookup_table_tx_count_after_start,
        lookup_table_tx_count_before + 1
    );

    // The pubkeys needed to commit the cloned account should be reserved when it was cloned
    // in a single lookup table transaction
    assert_eq!(
        lookup_table_tx_count_after_clone,
        lookup_table_tx_count_after_start + 1
    );
}
