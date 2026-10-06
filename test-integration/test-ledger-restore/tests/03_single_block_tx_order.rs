use std::{path::Path, process::Child};

use cleanass::assert_eq;
use integration_test_tools::{
    expect, tmpdir::resolve_tmp_dir, validator::cleanup,
};
use solana_message::{v0, v1, Message, VersionedMessage};
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL,
    rent::Rent,
    signature::{Keypair, Signer},
};
use solana_system_interface::instruction as system_instruction;
use solana_transaction::versioned::VersionedTransaction;
use test_ledger_restore::{
    airdrop_and_delegate_accounts, setup_offline_validator,
    setup_validator_with_local_remote, wait_for_ledger_persist, TMP_DIR_LEDGER,
};

const SLOT_MS: u64 = 150;

#[test]
fn test_restore_ledger_with_multiple_dependent_transactions_same_slot() {
    let (_tmpdir, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let (mut validator, _, keypairs) = write(&ledger_path, false);
    test_ledger_restore::kill_validator(&mut validator);

    let mut validator = read(&ledger_path, &keypairs);
    test_ledger_restore::kill_validator(&mut validator);
}

#[test]
fn test_restore_ledger_with_multiple_dependent_transactions_separate_slot() {
    let (_tmpdir, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let (mut validator, _, keypairs) = write(&ledger_path, true);
    test_ledger_restore::kill_validator(&mut validator);

    let mut validator = read(&ledger_path, &keypairs);
    test_ledger_restore::kill_validator(&mut validator);
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

    // Dependent transfers exercise legacy, v0 and v1 in durable execution order.
    let rpc = expect!(ctx.try_ephem_client(), validator);
    for (i, pair) in keypairs.windows(2).enumerate() {
        if separate_slot {
            slot += 1;
            expect!(ctx.wait_for_slot_ephem(slot), validator);
        }
        let payer = &pair[0];
        let ix = system_instruction::transfer(
            &payer.pubkey(),
            &pair[1].pubkey(),
            (4 - i) as u64 * LAMPORTS_PER_SOL,
        );
        let hash = expect!(rpc.get_latest_blockhash(), validator);
        let message = match i % 3 {
            0 => VersionedMessage::Legacy(Message::new_with_blockhash(
                &[ix],
                Some(&payer.pubkey()),
                &hash,
            )),
            1 => VersionedMessage::V0(expect!(
                v0::Message::try_compile(&payer.pubkey(), &[ix], &[], hash),
                validator
            )),
            _ => VersionedMessage::V1(expect!(
                v1::Message::try_compile_with_config(
                    &payer.pubkey(),
                    &[ix],
                    hash,
                    v1::TransactionConfig::default()
                        .with_compute_unit_limit(100_000)
                        .with_loaded_accounts_data_size_limit(1_000_000)
                ),
                validator
            )),
        };
        let transaction = expect!(
            VersionedTransaction::try_new(message, &[payer]),
            validator
        );
        expect!(rpc.send_and_confirm_transaction(&transaction), validator);
    }

    let slot = wait_for_ledger_persist(&ctx, &mut validator);

    (validator, slot, keypairs)
}

fn read(ledger_path: &Path, keypairs: &[Keypair]) -> Child {
    let (_, mut validator, ctx) =
        setup_offline_validator(ledger_path, None, Some(SLOT_MS), false, false);

    for keypair in keypairs {
        let acc = expect!(
            expect!(ctx.try_ephem_client(), validator)
                .get_account(&keypair.pubkey()),
            validator
        );
        // Gasless execution leaves exactly one SOL plus the original rent reserve.
        assert_eq!(
            acc.lamports,
            Rent::default().minimum_balance(0) + LAMPORTS_PER_SOL,
            cleanup(&mut validator)
        );
    }
    validator
}
