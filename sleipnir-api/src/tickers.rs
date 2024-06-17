use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use log::*;
use sleipnir_accounts::AccountsManager;
use sleipnir_bank::bank::Bank;
use sleipnir_ledger::Ledger;
use tokio_util::sync::CancellationToken;

pub fn init_slot_ticker(
    bank: &Arc<Bank>,
    ledger: Arc<Ledger>,
    tick_duration: Duration,
    exit: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    let bank = bank.clone();
    let log = tick_duration >= Duration::from_secs(5);
    std::thread::spawn(move || {
        while !exit.load(Ordering::Relaxed) {
            std::thread::sleep(tick_duration);
            let slot = bank.advance_slot();
            let _ = ledger
                .cache_block_time(slot, timestamp_in_secs() as i64)
                .map_err(|e| {
                    error!("Failed to cache block time: {:?}", e);
                });
            if log {
                info!("Advanced to slot {}", slot);
            }
        }
    })
}

pub fn init_commit_accounts_ticker(
    manager: &Arc<AccountsManager>,
    tick_duration: Duration,
    token: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let manager = manager.clone();
    tokio::task::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(tick_duration) => {
                    let sigs = manager.commit_delegated().await;
                    match sigs {
                        Ok(sigs) if sigs.is_empty() => {
                            trace!("No accounts committed");
                        }
                        Ok(sigs) => {
                            debug!("Commits: {:?}", sigs);
                        }
                        Err(err) => {
                            error!("Failed to commit accounts: {:?}", err);
                        }
                    }
                }
                _ = token.cancelled() => {
                    break;
                }
            }
        }
    })
}

fn timestamp_in_secs() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("create timestamp in timing");
    now.as_secs()
}
