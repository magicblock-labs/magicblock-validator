use cleanass::{assert, assert_eq};
use magicblock_config::LedgerResumeStrategy;
use std::{path::Path, process::Child};

use integration_test_tools::{
    expect, tmpdir::resolve_tmp_dir, unwrap, validator::cleanup,
};
use solana_sdk::{
    commitment_config::CommitmentConfig, pubkey::Pubkey, signature::Signature,
};
use test_ledger_restore::{
    setup_offline_validator, wait_for_ledger_persist, TMP_DIR_LEDGER,
};

#[test]
fn restore_ledger_with_airdropped_account() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let pubkey = Pubkey::new_unique();

    let (mut validator, airdrop_sig, _) = write_ledger(&ledger_path, &pubkey);
    validator.kill().unwrap();

    let mut validator = read_ledger(&ledger_path, &pubkey, Some(&airdrop_sig));
    validator.kill().unwrap();
}

fn write_ledger(
    ledger_path: &Path,
    pubkey1: &Pubkey,
) -> (Child, Signature, u64) {
    // Launch a validator and airdrop to an account
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        None,
        LedgerResumeStrategy::Reset,
    );

    let sig = expect!(ctx.airdrop_ephem(pubkey1, 1_111_111), validator);

    let lamports = expect!(ctx.fetch_ephem_account_balance(pubkey1), validator);
    assert_eq!(lamports, 1_111_111, cleanup(&mut validator));

    let slot = wait_for_ledger_persist(&mut validator);

    validator.kill().unwrap();
    (validator, sig, slot)
}

fn read_ledger(
    ledger_path: &Path,
    pubkey1: &Pubkey,
    airdrop_sig1: Option<&Signature>,
) -> Child {
    // Launch another validator reusing ledger
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        None,
        LedgerResumeStrategy::Replay,
    );

    let acc = expect!(
        expect!(ctx.try_ephem_client(), validator).get_account(pubkey1),
        validator
    );
    assert_eq!(acc.lamports, 1_111_111, cleanup(&mut validator));

    if let Some(sig) = airdrop_sig1 {
        let status = match expect!(ctx.try_ephem_client(), validator)
            .get_signature_status_with_commitment_and_history(
                sig,
                CommitmentConfig::confirmed(),
                true,
            ) {
            Ok(status) => {
                unwrap!(
                    status,
                    format!("Should have received signature status for {sig}"),
                    validator
                )
            }
            Err(err) => {
                cleanup(&mut validator);
                panic!("Error fetching signature status: {:?}", err);
            }
        };
        assert!(status.is_ok(), cleanup(&mut validator));
    }

    validator
}
