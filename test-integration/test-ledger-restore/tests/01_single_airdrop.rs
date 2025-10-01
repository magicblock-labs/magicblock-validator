use log::*;
use std::{path::Path, process::Child};
use test_kit::init_logger;

use cleanass::{assert, assert_eq};
use integration_test_tools::{
    expect, tmpdir::resolve_tmp_dir, unwrap, validator::cleanup,
};
use magicblock_config::LedgerResumeStrategy;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    system_instruction,
};
use test_ledger_restore::{
    setup_offline_validator, setup_validator_with_local_remote,
    wait_for_ledger_persist, TMP_DIR_LEDGER,
};

#[test]
fn test_restore_ledger_with_airdropped_account() {
    init_logger!();

    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let keypair1 = Keypair::new();
    let keypair2 = Keypair::new();

    let (mut validator, transfer_sig, _) =
        write_ledger(&ledger_path, &keypair1, &keypair2);
    validator.kill().unwrap();
    debug!("Transfer sig: {transfer_sig}");

    let mut validator =
        read_ledger(&ledger_path, &keypair2.pubkey(), Some(&transfer_sig));
    validator.kill().unwrap();
}

fn write_ledger(
    ledger_path: &Path,
    keypair1: &Keypair,
    keypair2: &Keypair,
) -> (Child, Signature, u64) {
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

    let payer_chain = Keypair::new();
    expect!(
        ctx.airdrop_chain(&payer_chain.pubkey(), LAMPORTS_PER_SOL),
        "Failed to airdrop to payer_chain",
        validator
    );
    expect!(
        ctx.airdrop_chain_and_delegate(&payer_chain, keypair1, 1_111_111),
        "Failed to airdrop and delegate  keypair1",
        validator
    );
    expect!(
        ctx.airdrop_chain_and_delegate(&payer_chain, keypair2, 2_222_222),
        "Failed to airdrop and delegate  keypair2",
        validator
    );

    let transfer_ix = system_instruction::transfer(
        &keypair1.pubkey(),
        &keypair2.pubkey(),
        111,
    );
    let (sig, _) = expect!(
        ctx.send_and_confirm_instructions_with_payer_ephem(
            &[transfer_ix],
            keypair1,
        ),
        "Failed to send transfer from keypair1 to keypair2",
        validator
    );

    let lamports = expect!(
        ctx.fetch_ephem_account_balance(&keypair2.pubkey()),
        validator
    );
    assert_eq!(lamports, 2_222_333, cleanup(&mut validator));

    let slot = wait_for_ledger_persist(&mut validator);

    validator.kill().unwrap();
    (validator, sig, slot)
}

fn read_ledger(
    ledger_path: &Path,
    pubkey2: &Pubkey,
    transfer_sig1: Option<&Signature>,
) -> Child {
    // Launch another validator reusing ledger
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        None,
        LedgerResumeStrategy::Resume { replay: true },
        false,
    );

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
