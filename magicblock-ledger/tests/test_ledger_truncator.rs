mod common;
use std::{sync::Arc, time::Duration};

use magicblock_ledger::{ledger_truncator::LedgerTruncator, Ledger};
use solana_sdk::{hash::Hash, signature::Signature};

use crate::common::{setup, write_dummy_transaction};

const TEST_TRUNCATION_TIME_INTERVAL: Duration = Duration::from_millis(50);

fn verify_transactions_state(
    ledger: &Ledger,
    start_slot: u64,
    signatures: &[Signature],
    shall_exist: bool,
) {
    for (offset, signature) in signatures.iter().enumerate() {
        let slot = start_slot + offset as u64;
        assert_eq!(
            ledger.read_slot_signature((slot, 0)).unwrap().is_some(),
            shall_exist
        );
        assert_eq!(
            ledger
                .read_transaction((*signature, slot))
                .unwrap()
                .is_some(),
            shall_exist
        );
        assert_eq!(
            ledger
                .read_transaction_status((*signature, slot))
                .unwrap()
                .is_some(),
            shall_exist
        )
    }
}

/// Tests that ledger is not truncated if finality slot - 0
#[tokio::test]
async fn test_truncator_not_purged_finality() {
    const SLOT_TRUNCATION_INTERVAL: u64 = 5;

    let ledger = Arc::new(setup());

    let mut ledger_truncator =
        LedgerTruncator::new(ledger.clone(), TEST_TRUNCATION_TIME_INTERVAL, 0);

    for i in 0..SLOT_TRUNCATION_INTERVAL {
        write_dummy_transaction(&ledger, i, 0);
        ledger.write_block(i, 0, Hash::new_unique()).unwrap()
    }
    let signatures = (0..SLOT_TRUNCATION_INTERVAL)
        .map(|i| {
            let signature = ledger.read_slot_signature((i, 0)).unwrap();
            assert!(signature.is_some());

            signature.unwrap()
        })
        .collect::<Vec<_>>();

    ledger_truncator.start();
    tokio::time::sleep(Duration::from_millis(10)).await;
    ledger_truncator.stop();
    assert!(ledger_truncator.join().await.is_ok());

    // Not truncated due to final_slot 0
    verify_transactions_state(&ledger, 0, &signatures, true);
}

// Tests that ledger is not truncated while there is still enough space
#[tokio::test]
async fn test_truncator_not_purged_size() {
    const NUM_TRANSACTIONS: u64 = 100;

    let ledger = Arc::new(setup());

    let mut ledger_truncator = LedgerTruncator::new(
        ledger.clone(),
        TEST_TRUNCATION_TIME_INTERVAL,
        1 << 30, // 1 GB
    );

    for i in 0..NUM_TRANSACTIONS {
        write_dummy_transaction(&ledger, i, 0);
        ledger.write_block(i, 0, Hash::new_unique()).unwrap()
    }
    let signatures = (0..NUM_TRANSACTIONS)
        .map(|i| {
            let signature = ledger.read_slot_signature((i, 0)).unwrap();
            assert!(signature.is_some());

            signature.unwrap()
        })
        .collect::<Vec<_>>();

    ledger_truncator.start();
    tokio::time::sleep(Duration::from_millis(10)).await;
    ledger_truncator.stop();
    assert!(ledger_truncator.join().await.is_ok());

    // Not truncated due to final_slot 0
    verify_transactions_state(&ledger, 0, &signatures, true);
}

// Tests that ledger got truncated but not after finality slot
#[tokio::test]
async fn test_truncator_non_empty_ledger() {
    const FINAL_SLOT: u64 = 80;

    let ledger = Arc::new(setup());
    let signatures = (0..FINAL_SLOT + 20)
        .map(|i| {
            let (_, signature) = write_dummy_transaction(&ledger, i, 0);
            ledger.write_block(i, 0, Hash::new_unique()).unwrap();
            signature
        })
        .collect::<Vec<_>>();

    let mut ledger_truncator =
        LedgerTruncator::new(ledger.clone(), TEST_TRUNCATION_TIME_INTERVAL, 0);

    ledger_truncator.start();
    tokio::time::sleep(TEST_TRUNCATION_TIME_INTERVAL).await;

    ledger_truncator.stop();
    assert!(ledger_truncator.join().await.is_ok());

    let cleanup_slot = ledger.get_lowest_cleanup_slot();
    assert_ne!(ledger.get_lowest_cleanup_slot(), 0);
    verify_transactions_state(
        &ledger,
        0,
        &signatures[..(cleanup_slot + 1) as usize],
        false,
    );
    verify_transactions_state(
        &ledger,
        cleanup_slot + 1,
        &signatures[(cleanup_slot + 1) as usize..],
        true,
    );
}

async fn transaction_spammer(
    ledger: Arc<Ledger>,
    num_of_iterations: usize,
    tx_per_operation: usize,
) -> Vec<Signature> {
    let mut signatures =
        Vec::with_capacity(num_of_iterations * tx_per_operation);
    for _ in 0..num_of_iterations {
        for _ in 0..tx_per_operation {
            let slot = signatures.len() as u64;
            let (_, signature) = write_dummy_transaction(&ledger, slot, 0);
            ledger.write_block(slot, 0, Hash::new_unique()).unwrap();
            signatures.push(signature);
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    signatures
}

// Tests if ledger truncated correctly during tx spamming with finality slot increments
#[tokio::test]
async fn test_truncator_with_tx_spammer() {
    let ledger = Arc::new(setup());

    let mut ledger_truncator =
        LedgerTruncator::new(ledger.clone(), TEST_TRUNCATION_TIME_INTERVAL, 0);

    ledger_truncator.start();
    let handle = tokio::spawn(transaction_spammer(ledger.clone(), 10, 20));

    // Sleep some time
    tokio::time::sleep(Duration::from_secs(3)).await;

    let signatures_result = handle.await;
    assert!(ledger.flush().is_ok());
    assert!(signatures_result.is_ok());
    let signatures = signatures_result.unwrap();

    // Stop truncator assuming that complete after sleep
    ledger_truncator.stop();
    assert!(ledger_truncator.join().await.is_ok());

    assert_eq!(ledger.get_lowest_cleanup_slot(), signatures.len() as u64);
    verify_transactions_state(&ledger, 0, &signatures, false);
}

#[ignore = "Long running test"]
#[tokio::test]
async fn test_with_1gb_db() {
    const DB_SIZE: u64 = 1 << 30;
    const CHECK_RATE: u64 = 100;

    // let ledger = Arc::new(Ledger::open(Path::new("/var/folders/r9/q7l5l9ks1vs1nlv10vlpkhw80000gn/T/.tmp00LEDc/rocksd")).unwrap());
    let ledger = Arc::new(setup());

    let mut slot = 0;
    loop {
        if slot % CHECK_RATE == 0 && ledger.storage_size().unwrap() >= DB_SIZE {
            break;
        }

        write_dummy_transaction(&ledger, slot, 0);
        ledger.write_block(slot, 0, Hash::new_unique()).unwrap();
        slot += 1
    }

    let mut ledger_truncator = LedgerTruncator::new(
        ledger.clone(),
        TEST_TRUNCATION_TIME_INTERVAL,
        DB_SIZE,
    );

    ledger_truncator.start();
    tokio::time::sleep(Duration::from_secs(1)).await;
    ledger_truncator.stop();

    ledger_truncator.join().await.unwrap();
}
