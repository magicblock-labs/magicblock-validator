use std::{path::Path, process::Child};

use cleanass::assert;
use integration_test_tools::{
    expect, loaded_accounts::LoadedAccounts, tmpdir::resolve_tmp_dir,
    validator::cleanup,
};
use program_flexi_counter::instruction::create_schedule_task_ix;
use solana_sdk::{signature::Keypair, signer::Signer};
use test_kit::init_logger;
use test_ledger_restore::{
    confirm_tx_with_payer_ephem, fetch_counter_ephem,
    init_and_delegate_counter_and_payer, setup_validator_with_local_remote,
    setup_validator_with_local_remote_and_resume_strategy,
    wait_for_ledger_persist, TMP_DIR_LEDGER,
};
use tracing::*;

#[test]
fn test_crank_persistence() {
    init_logger!();
    let (_tmpdir, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let (mut validator, payer, count) = write(&ledger_path);
    validator.kill().unwrap();

    let mut validator = read(&ledger_path, &payer, count);
    validator.kill().unwrap();
}

fn write(ledger_path: &Path) -> (Child, Keypair, u64) {
    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        None,
        true,
        false,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

    let (payer, _) =
        init_and_delegate_counter_and_payer(&ctx, &mut validator, "COUNTER");

    // Schedule a task
    let task_id = 1;
    let execution_interval_millis = 50;
    let iterations = 1000;
    let ix = create_schedule_task_ix(
        payer.pubkey(),
        task_id,
        execution_interval_millis,
        iterations,
        false,
        false,
    );
    confirm_tx_with_payer_ephem(ix, &payer, &ctx, &mut validator);

    // Wait for the task to be scheduled and executed
    expect!(ctx.wait_for_delta_slot_ephem(3), validator);

    // Check that the counter was incremented
    let counter_account =
        fetch_counter_ephem(&ctx, &payer.pubkey(), &mut validator);
    assert!(
        counter_account.count > 0,
        cleanup(&mut validator),
        "counter.count: {}",
        counter_account.count
    );

    // Wait more to be sure the ledger is persisted
    wait_for_ledger_persist(&ctx, &mut validator);
    debug!("✅ Ledger persisted");

    (validator, payer, counter_account.count)
}

fn read(ledger_path: &Path, kp: &Keypair, count: u64) -> Child {
    let (_, mut validator, ctx) =
        setup_validator_with_local_remote_and_resume_strategy(
            ledger_path,
            None,
            false,
            false,
            &LoadedAccounts::with_delegation_program_test_authority(),
        );

    // Check that the counter persisted its value
    let current_count =
        fetch_counter_ephem(&ctx, &kp.pubkey(), &mut validator).count;
    assert!(
        current_count >= count,
        cleanup(&mut validator),
        "counter.count: {} < {}",
        current_count,
        count
    );

    // Wait for the crank to execute
    expect!(ctx.wait_for_delta_slot_ephem(10), validator);

    // Check that the count increased
    let new_count =
        fetch_counter_ephem(&ctx, &kp.pubkey(), &mut validator).count;
    assert!(
        new_count > current_count,
        cleanup(&mut validator),
        "counter.count: {} <= {}",
        new_count,
        current_count
    );

    validator
}
