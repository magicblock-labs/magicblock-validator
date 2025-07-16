use cleanass::assert_eq;
use magicblock_config::TEST_SNAPSHOT_FREQUENCY;
use magicblock_ledger::Ledger;
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_transaction_status::{TransactionStatusMeta, UiTransactionEncoding};
use std::{
    path::Path,
    process::Child,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use integration_test_tools::{expect, tmpdir::resolve_tmp_dir};
use solana_sdk::{
    hash::Hash,
    signature::{Keypair, Signature},
    signer::Signer,
    system_instruction,
    transaction::{SanitizedTransaction, Transaction},
};
use test_ledger_restore::{
    cleanup, setup_offline_validator, wait_for_ledger_persist, TMP_DIR_LEDGER,
};

// In this test we ensure that we can optionally skip the replay of the ledger
// when restoring.
#[test]
fn restore_ledger_skip_replay() {
    let (_, ledger_path) = resolve_tmp_dir(TMP_DIR_LEDGER);

    let keypairs = (0..10).map(|_| Keypair::new()).collect::<Vec<_>>();

    let (mut validator, slot, signatures, tx_slot) =
        write(&ledger_path, &keypairs);
    validator.kill().unwrap();

    assert!(slot > TEST_SNAPSHOT_FREQUENCY);

    let mut validator = read(&ledger_path, &keypairs, &signatures, tx_slot);
    validator.kill().unwrap();
}

fn write(
    ledger_path: &Path,
    keypairs: &[Keypair],
) -> (Child, u64, Vec<Signature>, u64) {
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        Some(50),
        true,
        false,
        false,
    );

    for pubkey in keypairs.iter().map(|kp| kp.pubkey()) {
        expect!(ctx.airdrop_ephem(&pubkey, 1_111_111), validator);

        let lamports =
            expect!(ctx.fetch_ephem_account_balance(&pubkey), validator);
        assert_eq!(lamports, 1_111_111, cleanup(&mut validator));
    }

    // Corrupt the ledger by transferring with the wrong signer
    let ledger = expect!(Ledger::open(ledger_path), validator);
    let mut signatures = Vec::with_capacity(keypairs.len());
    let blockhashes = expect!(
        ctx.get_all_blockhashes_ephem().and_then(|hashes| hashes
            .last()
            .cloned()
            .ok_or(anyhow::anyhow!("no blockhash"))),
        validator
    );
    let blockhash = expect!(Hash::from_str(&blockhashes), validator);
    let tx_slot = expect!(
        ctx.try_ephem_client().and_then(|client| client
            .get_slot()
            .map_err(|e| anyhow::anyhow!("{}", e))),
        validator
    );
    for (i, kp) in keypairs.iter().enumerate() {
        let recipient = &keypairs[(i + 1) % keypairs.len()];

        let mut tx = Transaction::new_with_payer(
            &[system_instruction::transfer(
                &kp.pubkey(),
                &recipient.pubkey(),
                111_111,
            )],
            Some(&kp.pubkey()),
        );
        tx.partial_sign_unchecked(&[&recipient], vec![0], blockhash);
        let signature = tx.get_signature().clone();

        let sanitized_tx =
            SanitizedTransaction::from_transaction_for_tests(tx.clone());
        expect!(
            ledger.write_transaction(
                signature.clone(),
                tx_slot,
                sanitized_tx,
                TransactionStatusMeta::default(),
                0,
            ),
            validator
        );

        signatures.push(signature);
    }

    expect!(
        ledger.write_block(
            tx_slot,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
            blockhash,
        ),
        validator
    );

    // NOTE: This slows the test down a lot (500 * 50ms = 25s) and will
    // be improved once we can configure `FLUSH_ACCOUNTS_SLOT_FREQ`
    expect!(
        ctx.wait_for_delta_slot_ephem(TEST_SNAPSHOT_FREQUENCY),
        validator
    );

    let slot = wait_for_ledger_persist(&mut validator);

    (validator, slot, signatures, tx_slot)
}

fn read(
    ledger_path: &Path,
    keypairs: &[Keypair],
    signatures: &[Signature],
    tx_slot: u64,
) -> Child {
    let (_, mut validator, ctx) = setup_offline_validator(
        ledger_path,
        None,
        Some(50),
        false,
        false,
        true,
    );

    // Transactions should exist even without replay
    let ledger = expect!(Ledger::open(ledger_path), validator);
    for (kp, signature) in keypairs.iter().zip(signatures) {
        let lamports =
            expect!(ctx.fetch_ephem_account_balance(&kp.pubkey()), validator);
        assert_eq!(lamports, 1_111_111, cleanup(&mut validator));

        expect!(
            ctx.try_ephem_client().and_then(|client| client
                .get_transaction(signature, UiTransactionEncoding::Base58)
                .map_err(|e| anyhow::anyhow!("{}", e))),
            validator
        );
        let tx = expect!(
            ledger
                .read_transaction((signature.clone(), tx_slot))
                .map_err(|e| anyhow::anyhow!("{}", e))
                .and_then(|tx| tx
                    .ok_or_else(|| anyhow::anyhow!("transaction not found"))),
            validator
        );
        eprintln!("tx: {:?}", tx);
    }

    validator
}
