use std::{path::Path, process::Child};

use integration_test_tools::{
    expect, tmpdir::resolve_tmp_dir, IntegrationTestContext,
};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL,
    signature::{Keypair, Signer},
    system_instruction,
    transaction::Transaction,
};
use test_ledger_restore::{setup_offline_validator, TMP_DIR_LEDGER};

const SLOT_MS: u64 = 150;

#[test]
fn restore_ledger_with_multiple_dependent_transactions_same_slot() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let keypairs = vec![
        Keypair::new(),
        Keypair::new(),
        Keypair::new(),
        Keypair::new(),
        Keypair::new(),
    ];

    let (mut validator, slot) = write(&ledger_path, &keypairs, false);
    validator.kill().unwrap();

    let mut validator = read(&ledger_path, &keypairs, slot);
    validator.kill().unwrap();
}

#[test]
fn restore_ledger_with_multiple_dependent_transactions_separate_slot() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let keypairs = vec![
        Keypair::new(),
        Keypair::new(),
        Keypair::new(),
        Keypair::new(),
        Keypair::new(),
    ];

    let (mut validator, slot) = write(&ledger_path, &keypairs, true);
    validator.kill().unwrap();

    let mut validator = read(&ledger_path, &keypairs, slot);
    validator.kill().unwrap();
}

fn write(
    ledger_path: &Path,
    keypairs: &[Keypair],
    separate_slot: bool,
) -> (Child, u64) {
    fn transfer(
        validator: &mut Child,
        ctx: &IntegrationTestContext,
        from: &Keypair,
        to: &Keypair,
        amount: u64,
    ) {
        let ix =
            system_instruction::transfer(&from.pubkey(), &to.pubkey(), amount);
        let mut tx = Transaction::new_with_payer(&[ix], Some(&from.pubkey()));
        let signers = &[from];
        let (_, confirmed) = expect!(
            ctx.send_and_confirm_transaction_ephem(&mut tx, signers),
            validator
        );
        assert!(confirmed);
    }

    let (_, mut validator, ctx) =
        setup_offline_validator(ledger_path, None, Some(SLOT_MS), true);

    let mut slot = 1;
    expect!(ctx.wait_for_slot_ephem(slot), validator);

    // We are executing 5 transactions which fail if they execute in the wrong order
    // since the sender account is always created in the transaction right before the
    // transaction where it sends lamports

    // 1. Airdrop 5 SOL to first account
    expect!(
        ctx.airdrop_ephem(&keypairs[0].pubkey(), 5 * LAMPORTS_PER_SOL),
        validator
    );

    // 2. Transfer 4 SOL from first account to second account
    if separate_slot {
        slot += 1;
        ctx.wait_for_slot_ephem(slot).unwrap();
    }
    transfer(
        &mut validator,
        &ctx,
        &keypairs[0],
        &keypairs[1],
        4 * LAMPORTS_PER_SOL,
    );

    // 3. Transfer 3 SOL from second account to third account
    if separate_slot {
        slot += 1;
        ctx.wait_for_slot_ephem(slot).unwrap();
    }
    transfer(
        &mut validator,
        &ctx,
        &keypairs[1],
        &keypairs[2],
        3 * LAMPORTS_PER_SOL,
    );

    // 4. Transfer 2 SOL from third account to fourth account
    if separate_slot {
        slot += 1;
        ctx.wait_for_slot_ephem(slot).unwrap();
    }
    transfer(
        &mut validator,
        &ctx,
        &keypairs[2],
        &keypairs[3],
        2 * LAMPORTS_PER_SOL,
    );

    // 5. Transfer 1 SOL from fourth account to fifth account
    if separate_slot {
        slot += 1;
        ctx.wait_for_slot_ephem(slot).unwrap();
    }
    transfer(
        &mut validator,
        &ctx,
        &keypairs[3],
        &keypairs[4],
        LAMPORTS_PER_SOL,
    );

    let slot = expect!(ctx.wait_for_next_slot_ephem(), validator);

    (validator, slot)
}

fn read(ledger_path: &Path, keypairs: &[Keypair], slot: u64) -> Child {
    let (_, mut validator, ctx) =
        setup_offline_validator(ledger_path, None, Some(SLOT_MS), false);
    assert!(ctx.wait_for_slot_ephem(slot).is_ok());

    for keypair in keypairs {
        let acc =
            expect!(ctx.ephem_client.get_account(&keypair.pubkey()), validator);
        // Since we don't collect fees at this point each account ends up
        // with exactly 1 SOL.
        // In the future we need to adapt this to allow for a range, i.e.
        // 0.9 SOL <= lamports <= 1 SOL
        assert_eq!(acc.lamports, LAMPORTS_PER_SOL);
    }
    validator
}
