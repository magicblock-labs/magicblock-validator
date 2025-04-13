mod common;
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use magicblock_core::traits::FinalityProvider;
use magicblock_ledger::{ledger_truncator::LedgerTruncator, Ledger};
use solana_sdk::signature::Signature;

use crate::common::{setup, write_dummy_transaction};

const TEST_TRUNCATION_TIME_INTERVAL: Duration = Duration::from_millis(50);
#[derive(Default)]
pub struct TestFinalityProvider {
    pub latest_final_slot: AtomicU64,
}

impl FinalityProvider for TestFinalityProvider {
    fn get_latest_final_slot(&self) -> u64 {
        self.latest_final_slot.load(Ordering::Relaxed)
    }
}

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
    let finality_provider = TestFinalityProvider {
        latest_final_slot: 0.into(),
    };

    let mut ledger_truncator = LedgerTruncator::new(
        ledger.clone(),
        Arc::new(finality_provider),
        TEST_TRUNCATION_TIME_INTERVAL,
        0,
    );

    for i in 0..SLOT_TRUNCATION_INTERVAL {
        write_dummy_transaction(&ledger, i, 0);
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
    let finality_provider = TestFinalityProvider {
        latest_final_slot: 0.into(),
    };

    let mut ledger_truncator = LedgerTruncator::new(
        ledger.clone(),
        Arc::new(finality_provider),
        TEST_TRUNCATION_TIME_INTERVAL,
        1 << 30, // 1 GB
    );

    for i in 0..NUM_TRANSACTIONS {
        write_dummy_transaction(&ledger, i, 0);
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
    let signatures = (0..100)
        .map(|i| {
            let (_, signature) = write_dummy_transaction(&ledger, i, 0);
            signature
        })
        .collect::<Vec<_>>();

    let finality_provider = Arc::new(TestFinalityProvider {
        latest_final_slot: FINAL_SLOT.into(),
    });

    let mut ledger_truncator = LedgerTruncator::new(
        ledger.clone(),
        finality_provider,
        TEST_TRUNCATION_TIME_INTERVAL,
        0,
    );

    ledger_truncator.start();
    tokio::time::sleep(Duration::from_millis(10)).await;

    ledger_truncator.stop();
    assert!(ledger_truncator.join().await.is_ok());

    assert_eq!(ledger.get_lowest_cleanup_slot(), FINAL_SLOT - 1);
    verify_transactions_state(
        &ledger,
        0,
        &signatures[..FINAL_SLOT as usize],
        false,
    );
    verify_transactions_state(
        &ledger,
        FINAL_SLOT,
        &signatures[FINAL_SLOT as usize..],
        true,
    );
}

async fn transaction_spammer(
    ledger: Arc<Ledger>,
    finality_provider: Arc<TestFinalityProvider>,
    num_of_iterations: usize,
    tx_per_operation: usize,
) -> Vec<Signature> {
    let mut signatures =
        Vec::with_capacity(num_of_iterations * tx_per_operation);
    for _ in 0..num_of_iterations {
        for _ in 0..tx_per_operation {
            let (_, signature) =
                write_dummy_transaction(&ledger, signatures.len() as u64, 0);
            signatures.push(signature);
        }

        finality_provider
            .latest_final_slot
            .store(signatures.len() as u64 - 1, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    signatures
}

// Tests if ledger truncated correctly during tx spamming with finality slot increments
#[tokio::test]
async fn test_truncator_with_tx_spammer() {
    let ledger = Arc::new(setup());
    let finality_provider = Arc::new(TestFinalityProvider {
        latest_final_slot: 0.into(),
    });

    let mut ledger_truncator = LedgerTruncator::new(
        ledger.clone(),
        finality_provider.clone(),
        TEST_TRUNCATION_TIME_INTERVAL,
        0,
    );

    ledger_truncator.start();
    let handle = tokio::spawn(transaction_spammer(
        ledger.clone(),
        finality_provider.clone(),
        10,
        20,
    ));

    // Sleep some time
    tokio::time::sleep(Duration::from_secs(1)).await;

    let signatures_result = handle.await;
    assert!(signatures_result.is_ok());
    let signatures = signatures_result.unwrap();

    // Stop truncator assuming that complete after sleep
    ledger_truncator.stop();
    assert!(ledger_truncator.join().await.is_ok());

    ledger.flush();

    let lowest_existing =
        finality_provider.latest_final_slot.load(Ordering::Relaxed);
    assert_eq!(ledger.get_lowest_cleanup_slot(), lowest_existing - 1);
    verify_transactions_state(
        &ledger,
        0,
        &signatures[..lowest_existing as usize],
        false,
    );
    verify_transactions_state(
        &ledger,
        lowest_existing,
        &signatures[lowest_existing as usize..],
        true,
    );
}
