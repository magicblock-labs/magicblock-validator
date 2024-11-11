use integration_test_tools::expect;
use integration_test_tools::tmpdir::resolve_tmp_dir;
use sleipnir_config::ProgramConfig;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use std::path::Path;
use std::process::Child;
use test_ledger_restore::{setup_offline_validator, SLOT_WRITE_DELTA, SOLX_ID, TMP_DIR_LEDGER};

#[test]
fn restore_ledger_with_solx_post() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);
    let pubkey = Pubkey::new_unique();

    write_ledger(&ledger_path, &pubkey);
}

fn get_programs() -> Vec<ProgramConfig> {
    vec![ProgramConfig {
        id: SOLX_ID.try_into().unwrap(),
        path: "fixtures/elfs/solanax.so".to_string(),
    }]
}

fn write_ledger(
    ledger_path: &Path,
    pubkey1: &Pubkey,
) -> (Child, Signature, u64) {
    let programs = get_programs();
    let (_, mut validator, ctx) =
        setup_offline_validator(ledger_path, Some(programs), true);

    let sig = expect!(ctx.airdrop_ephem(pubkey1, 1_111_111), validator);

    let lamports = expect!(ctx.fetch_ephem_account_balance(pubkey1), validator);
    assert_eq!(lamports, 1_111_111);

    let slot = ctx.wait_for_delta_slot_ephem(SLOT_WRITE_DELTA).unwrap();

    validator.kill().unwrap();
    (validator, sig, slot)
}
