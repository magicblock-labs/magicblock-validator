mod common;
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use magicblock_ledger::{
    ledger_purgatory::{FinalityProvider, LedgerPurgatory},
    Ledger,
};
use solana_sdk::signature::Signature;

use crate::common::{setup, write_dummy_transaction};

const TEST_PURGE_TIME_INTERVAL: Duration = Duration::from_millis(50);
#[derive(Default, Clone)]
pub struct TestFinalityProvider {
    latest_final_slot: Arc<AtomicU64>,
}

impl TestFinalityProvider {
    pub fn new(latest_final_slot: Arc<AtomicU64>) -> Self {
        Self { latest_final_slot }
    }
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

#[tokio::test]
async fn test_purgatory_not_purged() {
    const SLOT_PURGE_INTERVAL: u64 = 5;

    let ledger = Arc::new(setup());
    let latest_final_slot = Arc::new(AtomicU64::new(0));
    let finality_provider =
        TestFinalityProvider::new(latest_final_slot.clone());

    let mut ledger_purgatory = LedgerPurgatory::new(
        ledger.clone(),
        finality_provider,
        SLOT_PURGE_INTERVAL,
        TEST_PURGE_TIME_INTERVAL,
        1000,
    );

    for i in 0..SLOT_PURGE_INTERVAL {
        write_dummy_transaction(&ledger, i, 0);
    }
    let signatures = (0..SLOT_PURGE_INTERVAL)
        .map(|i| {
            let signature = ledger.read_slot_signature((i, 0)).unwrap();
            assert!(signature.is_some());

            signature.unwrap()
        })
        .collect::<Vec<_>>();

    ledger_purgatory.start();
    tokio::time::sleep(Duration::from_millis(10)).await;
    ledger_purgatory.stop();
    ledger_purgatory.join().await;
    // TODO: maybe replace with .stop(), .join() & .start() again?

    // Not purged due to final_slot 0
    verify_transactions_state(&ledger, 0, &signatures, true);
}

#[tokio::test]
async fn test_purgatory_non_empty_ledger() {
    const SLOT_PURGE_INTERVAL: u64 = 5;
    const FINAL_SLOT: u64 = 80;
    const LOWEST_EXISTING_SLOT: u64 = FINAL_SLOT - SLOT_PURGE_INTERVAL;

    let ledger = Arc::new(setup());
    let signatures = (0..100)
        .map(|i| {
            let (_, signature) = write_dummy_transaction(&ledger, i, 0);
            signature
        })
        .collect::<Vec<_>>();

    let finality_provider =
        TestFinalityProvider::new(Arc::new(FINAL_SLOT.into()));

    let mut ledger_purgatory = LedgerPurgatory::new(
        ledger.clone(),
        finality_provider,
        SLOT_PURGE_INTERVAL,
        TEST_PURGE_TIME_INTERVAL,
        1000,
    );

    ledger_purgatory.start();
    tokio::time::sleep(Duration::from_millis(10)).await;

    ledger_purgatory.stop();
    ledger_purgatory.join().await;

    assert_eq!(ledger.get_lowest_cleanup_slot(), LOWEST_EXISTING_SLOT - 1);
    verify_transactions_state(
        &ledger,
        0,
        &signatures[..LOWEST_EXISTING_SLOT as usize],
        false,
    );
    verify_transactions_state(
        &ledger,
        LOWEST_EXISTING_SLOT,
        &signatures[LOWEST_EXISTING_SLOT as usize..],
        true,
    );
}

async fn transaction_spammer(
    ledger: Arc<Ledger>,
    finality_slot: Arc<AtomicU64>,
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

        finality_slot.store(signatures.len() as u64 - 1, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    signatures
}

#[tokio::test]
async fn test_purgatory_with_tx_spammer() {
    const SLOT_PURGE_INTERVAL: u64 = 5;

    let ledger = Arc::new(setup());
    let latest_final_slot = Arc::new(AtomicU64::new(0));
    let finality_provider =
        TestFinalityProvider::new(latest_final_slot.clone());

    let mut ledger_purgatory = LedgerPurgatory::new(
        ledger.clone(),
        finality_provider,
        SLOT_PURGE_INTERVAL,
        TEST_PURGE_TIME_INTERVAL,
        1000,
    );

    ledger_purgatory.start();
    let handle = tokio::spawn(transaction_spammer(
        ledger.clone(),
        latest_final_slot.clone(),
        10,
        20,
    ));

    // Sleep some time
    tokio::time::sleep(Duration::from_secs(1)).await;

    let signatures_result = handle.await;
    assert!(signatures_result.is_ok());
    let signatures = signatures_result.unwrap();

    // Stop purgatory assuming that complete after sleep
    ledger_purgatory.stop();
    ledger_purgatory.join().await;

    ledger.flush();

    let lowest_existing =
        (latest_final_slot.load(Ordering::Relaxed) - SLOT_PURGE_INTERVAL);
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
