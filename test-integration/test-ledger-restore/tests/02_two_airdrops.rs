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
fn restore_ledger_with_two_airdropped_accounts_same_slot() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let pubkey1 = Pubkey::new_unique();
    let pubkey2 = Pubkey::new_unique();

    let (mut validator, airdrop_sig1, airdrop_sig2, slot) =
        write(&ledger_path, &pubkey1, &pubkey2, false);
    validator.kill().unwrap();

    let mut validator = read(
        &ledger_path,
        &pubkey1,
        &pubkey2,
        Some(&airdrop_sig1),
        Some(&airdrop_sig2),
        slot,
    );
    validator.kill().unwrap();
}

#[test]
fn restore_ledger_with_two_airdropped_accounts_separate_slot() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let pubkey1 = Pubkey::new_unique();
    let pubkey2 = Pubkey::new_unique();

    let (mut validator, airdrop_sig1, airdrop_sig2, slot) =
        write(&ledger_path, &pubkey1, &pubkey2, true);
    validator.kill().unwrap();

    let mut validator = read(
        &ledger_path,
        &pubkey1,
        &pubkey2,
        Some(&airdrop_sig1),
        Some(&airdrop_sig2),
        slot,
    );
    validator.kill().unwrap();
}

fn write(
    ledger_path: &Path,
    pubkey1: &Pubkey,
    pubkey2: &Pubkey,
    separate_slot: bool,
) -> (Child, Signature, Signature, u64) {
    let (_, mut validator, ctx) = setup_offline_validator(ledger_path, None, true);

    let mut slot = 5;
    ctx.wait_for_slot_ephem(slot).unwrap();
    let sig1 = expect!(ctx.airdrop_ephem(pubkey1, 1_111_111), validator);

    if separate_slot {
        slot += 5;
        ctx.wait_for_slot_ephem(slot).unwrap();
    }
    let sig2 = expect!(ctx.airdrop_ephem(pubkey2, 2_222_222), validator);

    let lamports1 =
        expect!(ctx.fetch_ephem_account_balance(pubkey1), validator);
    assert_eq!(lamports1, 1_111_111);

    let lamports2 =
        expect!(ctx.fetch_ephem_account_balance(pubkey2), validator);
    assert_eq!(lamports2, 2_222_222);

    slot += SLOT_WRITE_DELTA;
    let slot = ctx.wait_for_slot_ephem(slot).unwrap();

    (validator, sig1, sig2, slot)
}

fn read(
    ledger_path: &Path,
    pubkey1: &Pubkey,
    pubkey2: &Pubkey,
    airdrop_sig1: Option<&Signature>,
    airdrop_sig2: Option<&Signature>,
    slot: u64,
) -> Child {
    let (_, mut validator, ctx) = setup_offline_validator(ledger_path, None, false);
    assert!(ctx.wait_for_slot_ephem(slot).is_ok());

    let acc1 = expect!(ctx.ephem_client.get_account(pubkey1), validator);
    assert_eq!(acc1.lamports, 1_111_111);

    let acc2 = expect!(ctx.ephem_client.get_account(pubkey2), validator);
    assert_eq!(acc2.lamports, 2_222_222);

    if let Some(sig) = airdrop_sig1 {
        let status =
            ctx.ephem_client.get_signature_status(sig).unwrap().unwrap();
        assert!(status.is_ok());
    }

    if let Some(sig) = airdrop_sig2 {
        let status =
            ctx.ephem_client.get_signature_status(sig).unwrap().unwrap();
        assert!(status.is_ok());
    }
    validator
}

// -----------------
// Diagnose
// -----------------
// Uncomment either of the below to run ledger write/read in isolation and
// optionally keep the validator running after reading the ledger

// #[test]
fn _diagnose_write() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let pubkey1 = Pubkey::new_unique();
    let pubkey2 = Pubkey::new_unique();

    let (mut validator, airdrop_sig1, airdrop_sig2, slot) =
        write(&ledger_path, &pubkey1, &pubkey2, true);

    eprintln!("{}", ledger_path.display());
    eprintln!("{}: {:?}", pubkey1, airdrop_sig1);
    eprintln!("{}: {:?}", pubkey2, airdrop_sig2);
    eprintln!("slot: {}", slot);

    validator.kill().unwrap();
}

// #[test]
fn _diagnose_read() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let pubkey1 = Pubkey::new_unique();
    let pubkey2 = Pubkey::new_unique();

    eprintln!("{}", ledger_path.display());
    eprintln!("{}", pubkey1);
    eprintln!("{}", pubkey2);

    let (_, mut _validator, _ctx) =
        setup_offline_validator(&ledger_path, None, false);
}
