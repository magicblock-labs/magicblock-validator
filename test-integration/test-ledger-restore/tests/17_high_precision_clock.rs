use std::{path::Path, process::Child};

use cleanass::{assert, assert_eq};
use integration_test_tools::{
    loaded_accounts::LoadedAccounts, tmpdir::resolve_tmp_dir,
    validator::cleanup,
};
use program_flexi_counter::{
    instruction::create_record_high_precision_clock_ix, state::FlexiCounter,
};
use solana_sdk::{signature::Keypair, signer::Signer};
use test_kit::init_logger;
use test_ledger_restore::{
    confirm_tx_with_payer_ephem, fetch_counter_ephem,
    init_and_delegate_counter_and_payer, setup_validator_with_local_remote,
    wait_for_counter_ephem_state, wait_for_ledger_persist, TMP_DIR_LEDGER,
};
use tracing::*;

const COUNTER: &str = "HighPrecisionClock Counter";

#[test]
fn test_restore_ledger_reproduces_high_precision_clock() {
    init_logger!();
    let (_tmpdir, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let (mut validator, payer, observed) = write(&ledger_path);
    test_ledger_restore::kill_validator(&mut validator);

    let mut validator = read(&ledger_path, &payer, &observed);
    test_ledger_restore::kill_validator(&mut validator);
}

fn write(ledger_path: &Path) -> (Child, Keypair, FlexiCounter) {
    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        None,
        true,
        false,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

    let (payer, _counter) =
        init_and_delegate_counter_and_payer(&ctx, &mut validator, COUNTER);

    // Record the HighPrecisionClock sysvar value observed during execution.
    let ix = create_record_high_precision_clock_ix(payer.pubkey());
    confirm_tx_with_payer_ephem(ix, &payer, &ctx, &mut validator);

    let observed = fetch_counter_ephem(&ctx, &payer.pubkey(), &mut validator);
    debug!(
        "✅ Recorded high precision clock: nanos={}, unix_timestamp={}",
        observed.count, observed.updates
    );

    // Sanity: the sub-second component (stored in `count`) must be a valid
    // fraction of a second, confirming a real HighPrecisionClock was read.
    assert!(
        observed.count < 1_000_000_000,
        cleanup(&mut validator),
        "nanos out of range: {}",
        observed.count
    );
    assert!(
        observed.updates > 0,
        cleanup(&mut validator),
        "unix_timestamp should be set: {}",
        observed.updates
    );
    assert_eq!(observed.label, COUNTER.to_string(), cleanup(&mut validator));

    let slot = wait_for_ledger_persist(&ctx, &mut validator);
    debug!("✅ Ledger persisted at slot {slot}");

    (validator, payer, observed)
}

fn read(ledger_path: &Path, payer: &Keypair, expected: &FlexiCounter) -> Child {
    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        None,
        false,
        false,
        &LoadedAccounts::with_delegation_program_test_authority(),
    );

    // The replayed transaction must reproduce the exact observed sysvar value.
    wait_for_counter_ephem_state(
        &ctx,
        &mut validator,
        &payer.pubkey(),
        expected,
    );

    let restored = fetch_counter_ephem(&ctx, &payer.pubkey(), &mut validator);
    assert_eq!(restored, *expected, cleanup(&mut validator));
    debug!(
        "✅ Verified high precision clock reproduced after replay: nanos={}, unix_timestamp={}",
        restored.count, restored.updates
    );

    // Record the sysvar again, now on the freshly restarted validator, to prove
    // the HighPrecisionClock keeps advancing (not frozen at the replayed value).
    let ix = create_record_high_precision_clock_ix(payer.pubkey());
    confirm_tx_with_payer_ephem(ix, &payer, &ctx, &mut validator);

    let observed = fetch_counter_ephem(&ctx, &payer.pubkey(), &mut validator);
    debug!(
        "✅ Recorded high precision clock after restart: nanos={}, unix_timestamp={}",
        observed.count, observed.updates
    );

    // Sanity: the sub-second component (stored in `count`) must be a valid
    // fraction of a second, confirming a real HighPrecisionClock was read.
    assert!(
        observed.count < 1_000_000_000,
        cleanup(&mut validator),
        "nanos out of range: {}",
        observed.count
    );
    assert_eq!(observed.label, COUNTER.to_string(), cleanup(&mut validator));

    assert!(
        observed.updates > expected.updates || observed.count > expected.count,
        cleanup(&mut validator),
        "high precision clock should advance after restart: before={}, after={}",
        expected.updates,
        observed.updates
    );
    debug!("✅ Verified high precision clock advances after restart");

    validator
}
