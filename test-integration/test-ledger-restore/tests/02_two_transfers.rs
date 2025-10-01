use std::{path::Path, process::Child};

use cleanass::{assert, assert_eq};
use integration_test_tools::{
    expect, tmpdir::resolve_tmp_dir, unwrap, validator::cleanup,
};
use magicblock_config::LedgerResumeStrategy;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
};
use test_kit::Signer;
use test_ledger_restore::{
    airdrop_and_delegate_accounts, setup_offline_validator,
    setup_validator_with_local_remote, transfer_lamports,
    wait_for_ledger_persist, TMP_DIR_LEDGER,
};

#[test]
fn test_restore_ledger_with_two_airdropped_accounts_same_slot() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let (
        mut validator,
        transfer_sig1,
        transfer_sig2,
        _,
        _keypair1,
        keypair2,
        keypair3,
    ) = write(&ledger_path, false);
    validator.kill().unwrap();

    let mut validator = read(
        &ledger_path,
        &keypair2.pubkey(),
        &keypair3.pubkey(),
        Some(&transfer_sig1),
        Some(&transfer_sig2),
    );
    validator.kill().unwrap();
}

#[test]
fn test_restore_ledger_with_two_airdropped_accounts_separate_slot() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let (
        mut validator,
        transfer_sig1,
        transfer_sig2,
        _,
        _keypair1,
        keypair2,
        keypair3,
    ) = write(&ledger_path, true);
    validator.kill().unwrap();

    let mut validator = read(
        &ledger_path,
        &keypair2.pubkey(),
        &keypair3.pubkey(),
        Some(&transfer_sig1),
        Some(&transfer_sig2),
    );
    validator.kill().unwrap();
}

fn write(
    ledger_path: &Path,
    separate_slot: bool,
) -> (Child, Signature, Signature, u64, Keypair, Keypair, Keypair) {
    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        None,
        true,
        true,
        &Default::default(),
    );

    let mut keypairs = airdrop_and_delegate_accounts(
        &ctx,
        &mut validator,
        &[1_111_111, 2_222_222, 3_333_333],
    );
    let keypair1 = keypairs.drain(0..1).next().unwrap();
    let keypair2 = keypairs.drain(0..1).next().unwrap();
    let keypair3 = keypairs.drain(0..1).next().unwrap();

    let mut slot = 5;
    expect!(ctx.wait_for_slot_ephem(slot), validator);
    let sig1 = transfer_lamports(
        &ctx,
        &mut validator,
        &keypair1,
        &keypair2.pubkey(),
        111,
    );

    if separate_slot {
        slot += 5;
        ctx.wait_for_slot_ephem(slot).unwrap();
    }
    let sig2 = transfer_lamports(
        &ctx,
        &mut validator,
        &keypair1,
        &keypair3.pubkey(),
        111,
    );

    let lamports1 = expect!(
        ctx.fetch_ephem_account_balance(&keypair2.pubkey()),
        validator
    );
    assert_eq!(lamports1, 2_222_333, cleanup(&mut validator));

    let lamports2 = expect!(
        ctx.fetch_ephem_account_balance(&keypair3.pubkey()),
        validator
    );
    assert_eq!(lamports2, 3_333_444, cleanup(&mut validator));

    let slot = wait_for_ledger_persist(&mut validator);

    (validator, sig1, sig2, slot, keypair1, keypair2, keypair3)
}

fn read(
    ledger_path: &Path,
    pubkey1: &Pubkey,
    pubkey2: &Pubkey,
    transfer_sig1: Option<&Signature>,
    transfer_sig2: Option<&Signature>,
) -> Child {
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        None,
        LedgerResumeStrategy::Resume { replay: true },
        false,
    );

    let ephem_client = expect!(ctx.try_ephem_client(), validator);
    let acc1 = expect!(ephem_client.get_account(pubkey1), validator);
    assert_eq!(acc1.lamports, 2_222_333, cleanup(&mut validator));

    let acc2 = expect!(ephem_client.get_account(pubkey2), validator);
    assert_eq!(acc2.lamports, 3_333_444, cleanup(&mut validator));

    if let Some(sig) = transfer_sig1 {
        let status = {
            let res = expect!(
                ephem_client.get_signature_status_with_commitment_and_history(
                    sig,
                    CommitmentConfig::confirmed(),
                    true,
                ),
                validator
            );
            unwrap!(res, validator)
        };
        assert!(status.is_ok(), cleanup(&mut validator));
    }

    if let Some(sig) = transfer_sig2 {
        let status = {
            let res = expect!(
                ephem_client.get_signature_status_with_commitment_and_history(
                    sig,
                    CommitmentConfig::confirmed(),
                    true,
                ),
                validator
            );
            unwrap!(res, validator)
        };
        assert!(status.is_ok(), cleanup(&mut validator));
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

    let (mut validator, transfer_sig1, transfer_sig2, slot, kp1, kp2, kp3) =
        write(&ledger_path, true);

    eprintln!("{}", ledger_path.display());
    eprintln!("{} -> {}: {:?}", kp1.pubkey(), kp2.pubkey(), transfer_sig1);
    eprintln!("{} -> {}: {:?}", kp1.pubkey(), kp3.pubkey(), transfer_sig2);
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

    let (_, mut _validator, _ctx) = setup_offline_validator(
        &ledger_path,
        None,
        None,
        LedgerResumeStrategy::Resume { replay: true },
        false,
    );
}
