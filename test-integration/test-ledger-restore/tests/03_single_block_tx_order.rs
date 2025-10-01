use std::{path::Path, process::Child};

use cleanass::assert_eq;
use integration_test_tools::{
    expect, tmpdir::resolve_tmp_dir, validator::cleanup,
};
use magicblock_config::LedgerResumeStrategy;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL,
    rent::Rent,
    signature::{Keypair, Signer},
};
use test_ledger_restore::{
    airdrop_and_delegate_accounts, setup_offline_validator,
    setup_validator_with_local_remote, transfer_lamports,
    wait_for_ledger_persist, TMP_DIR_LEDGER,
};

const SLOT_MS: u64 = 150;

#[test]
fn test_restore_ledger_with_multiple_dependent_transactions_same_slot() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let (mut validator, _, keypairs) = write(&ledger_path, false);
    validator.kill().unwrap();

    let mut validator = read(&ledger_path, &keypairs);
    validator.kill().unwrap();
}

#[test]
fn test_restore_ledger_with_multiple_dependent_transactions_separate_slot() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let (mut validator, _, keypairs) = write(&ledger_path, true);
    validator.kill().unwrap();

    let mut validator = read(&ledger_path, &keypairs);
    validator.kill().unwrap();
}

fn write(
    ledger_path: &Path,
    separate_slot: bool,
) -> (Child, u64, Vec<Keypair>) {
    let (_, mut validator, ctx) = setup_validator_with_local_remote(
        ledger_path,
        None,
        true,
        true,
        &Default::default(),
    );

    let mut slot = 1;
    expect!(ctx.wait_for_slot_ephem(slot), validator);

    // We are executing 5 transactions which fail if they execute in the wrong order
    // since the sender account is transferred lamports to in the transaction right before the
    // transaction where it sends lamports to the next account.
    // The transfers are such that the account would not have enough lamports to send if the
    // transactions were to execute out of order.

    // 1. Airdrop 5 SOL to first account and only rent exempt the rest
    let mut lamports = vec![Rent::default().minimum_balance(0); 5];
    lamports[0] += 5 * LAMPORTS_PER_SOL;
    let keypairs =
        airdrop_and_delegate_accounts(&ctx, &mut validator, &lamports);

    // 2. Transfer 4 SOL from first account to second account
    if separate_slot {
        slot += 1;
        expect!(ctx.wait_for_slot_ephem(slot), validator);
    }
    transfer_lamports(
        &ctx,
        &mut validator,
        &keypairs[0],
        &keypairs[1].pubkey(),
        4 * LAMPORTS_PER_SOL,
    );

    // 3. Transfer 3 SOL from second account to third account
    if separate_slot {
        slot += 1;
        expect!(ctx.wait_for_slot_ephem(slot), validator);
    }
    transfer_lamports(
        &ctx,
        &mut validator,
        &keypairs[1],
        &keypairs[2].pubkey(),
        3 * LAMPORTS_PER_SOL,
    );

    // 4. Transfer 2 SOL from third account to fourth account
    if separate_slot {
        slot += 1;
        expect!(ctx.wait_for_slot_ephem(slot), validator);
    }
    transfer_lamports(
        &ctx,
        &mut validator,
        &keypairs[2],
        &keypairs[3].pubkey(),
        2 * LAMPORTS_PER_SOL,
    );

    // 5. Transfer 1 SOL from fourth account to fifth account
    if separate_slot {
        slot += 1;
        expect!(ctx.wait_for_slot_ephem(slot), validator);
    }
    transfer_lamports(
        &ctx,
        &mut validator,
        &keypairs[3],
        &keypairs[4].pubkey(),
        LAMPORTS_PER_SOL,
    );

    let slot = wait_for_ledger_persist(&mut validator);

    (validator, slot, keypairs)
}

fn read(ledger_path: &Path, keypairs: &[Keypair]) -> Child {
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        Some(SLOT_MS),
        LedgerResumeStrategy::Resume { replay: true },
        false,
    );

    for keypair in keypairs {
        let acc = expect!(
            expect!(ctx.try_ephem_client(), validator)
                .get_account(&keypair.pubkey()),
            validator
        );
        // Since we don't collect fees at this point each account ends up
        // with exactly 1 SOL.
        // In the future we need to adapt this to allow for a range, i.e.
        // 0.9 SOL <= lamports <= 1 SOL
        assert_eq!(
            acc.lamports,
            Rent::default().minimum_balance(0) + LAMPORTS_PER_SOL,
            cleanup(&mut validator)
        );
    }
    validator
}
