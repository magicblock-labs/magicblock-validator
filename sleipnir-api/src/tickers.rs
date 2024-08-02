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
    accounts_manager: &Arc<AccountsManager>,
    ledger: Arc<Ledger>,
    tick_duration: Duration,
    exit: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    let bank = bank.clone();
    let accounts_manager = accounts_manager.clone();
    let log = tick_duration >= Duration::from_secs(5);
    tokio::task::spawn(async move {
        while !exit.load(Ordering::Relaxed) {
            tokio::time::sleep(tick_duration).await;
            let slot = bank.advance_slot();
            let _ = ledger
                .cache_block_time(slot, timestamp_in_secs() as i64)
                .map_err(|e| {
                    error!("Failed to cache block time: {:?}", e);
                });
            let _ = accounts_manager.process_scheduled_commits().await.map_err(
                |e| {
                    error!("Failed to process scheduled commits: {:?}", e);
                },
            );
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
