use std::{path::Path, process::Child};

use cleanass::{assert, assert_eq};
use integration_test_tools::{
    expect, tmpdir::resolve_tmp_dir, unwrap, validator::cleanup,
};
use log::*;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
};
use test_kit::init_logger;
use test_ledger_restore::{
    airdrop_and_delegate_accounts, setup_offline_validator,
    setup_validator_with_local_remote, transfer_lamports,
    wait_for_ledger_persist, TMP_DIR_LEDGER,
};

#[test]
fn test_restore_ledger_with_transferred_account() {
    init_logger!();

    let (_tmpdir, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let (mut validator, transfer_sig, _slot, _keypair1, keypair2) =
        write_ledger(&ledger_path);
    validator.kill().unwrap();
    debug!("Transfer sig: {transfer_sig}");

    let mut validator =
        read_ledger(&ledger_path, &keypair2.pubkey(), Some(&transfer_sig));
    validator.kill().unwrap();
}

fn write_ledger(
    ledger_path: &Path,
) -> (Child, Signature, u64, Keypair, Keypair) {
    // Launch a validator and airdrop to an account
    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        None,
        true,
        true,
        &Default::default(),
    );

    // Wait to make sure we don't process transactions on slot 0
    expect!(ctx.wait_for_next_slot_ephem(), validator);

    let mut keypairs = airdrop_and_delegate_accounts(
        &ctx,
        &mut validator,
        &[1_111_111, 2_222_222],
    );
    let keypair1 = keypairs.drain(0..1).next().unwrap();
    let keypair2 = keypairs.drain(0..1).next().unwrap();

    let sig = transfer_lamports(
        &ctx,
        &mut validator,
        &keypair1,
        &keypair2.pubkey(),
        111,
    );

    let lamports = expect!(
        ctx.fetch_ephem_account_balance(&keypair2.pubkey()),
        validator
    );
    assert_eq!(lamports, 2_222_333, cleanup(&mut validator));

    let slot = wait_for_ledger_persist(&ctx, &mut validator);

    validator.kill().unwrap();
    (validator, sig, slot, keypair1, keypair2)
}

fn read_ledger(
    ledger_path: &Path,
    pubkey2: &Pubkey,
    transfer_sig1: Option<&Signature>,
) -> Child {
    // Launch another validator reusing ledger
    let (_, mut validator, ctx) =
        setup_offline_validator(ledger_path, None, None, false, false);

    let acc = expect!(
        expect!(ctx.try_ephem_client(), validator).get_account(pubkey2),
        validator
    );
    assert_eq!(acc.lamports, 2_222_333, cleanup(&mut validator));

    if let Some(sig) = transfer_sig1 {
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
