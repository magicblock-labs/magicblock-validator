use integration_test_tools::expect;
use integration_test_tools::tmpdir::resolve_tmp_dir;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use std::path::Path;
use std::process::Child;
use test_ledger_restore::{
    setup_offline_validator, SLOT_WRITE_DELTA, TMP_DIR_LEDGER,
};

#[test]
fn restore_ledger_with_airdropped_account() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let pubkey = Pubkey::new_unique();

    let (mut validator, airdrop_sig, slot) =
        write_ledger(&ledger_path, &pubkey);
    validator.kill().unwrap();

    let mut validator =
        read_ledger(&ledger_path, &pubkey, Some(&airdrop_sig), slot);
    validator.kill().unwrap();
}

fn write_ledger(
    ledger_path: &Path,
    pubkey1: &Pubkey,
) -> (Child, Signature, u64) {
    // Launch a validator and airdrop to an account
    let (_, mut validator, ctx) = setup_offline_validator(ledger_path, None, true);

    let sig = expect!(ctx.airdrop_ephem(pubkey1, 1_111_111), validator);

    let lamports = expect!(ctx.fetch_ephem_account_balance(pubkey1), validator);
    assert_eq!(lamports, 1_111_111);

    let slot = ctx.wait_for_delta_slot_ephem(SLOT_WRITE_DELTA).unwrap();

    validator.kill().unwrap();
    (validator, sig, slot)
}

fn read_ledger(
    ledger_path: &Path,
    pubkey1: &Pubkey,
    airdrop_sig1: Option<&Signature>,
    slot: u64,
) -> Child {
    // Launch another validator reusing ledger
    let (_, mut validator, ctx) = setup_offline_validator(ledger_path, None, false);
    assert!(ctx.wait_for_slot_ephem(slot).is_ok());

    let acc = expect!(ctx.ephem_client.get_account(pubkey1), validator);
    assert_eq!(acc.lamports, 1_111_111);

    if let Some(sig) = airdrop_sig1 {
        let status =
            ctx.ephem_client.get_signature_status(sig).unwrap().unwrap();
        assert!(status.is_ok());
    }

    validator
}
