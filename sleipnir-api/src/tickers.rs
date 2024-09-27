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
use sleipnir_core::magic_program;
use sleipnir_ledger::Ledger;
use sleipnir_processor::execute_transaction::execute_legacy_transaction;
use sleipnir_program::{
    sleipnir_instruction::accept_scheduled_commits, MagicContext,
};
use sleipnir_transaction_status::TransactionStatusSender;
use solana_sdk::account::ReadableAccount;
use tokio_util::sync::CancellationToken;

pub fn init_slot_ticker(
    bank: &Arc<Bank>,
    accounts_manager: &Arc<AccountsManager>,
    transaction_status_sender: Option<TransactionStatusSender>,
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

            // If accounts were scheduled to be committed, we accept them here
            // and processs the commits
            let magic_context_acc = bank.get_account(&magic_program::MAGIC_CONTEXT_PUBKEY)
                .expect("Validator found to be running without MagicContext account!");

            if MagicContext::has_scheduled_commits(magic_context_acc.data()) {
                // 1. Send the transaction to move the scheduled commits from the MagicContext
                //    to the global ScheduledCommit store
                let tx = accept_scheduled_commits(bank.last_blockhash());
                if let Err(err) = execute_legacy_transaction(
                    tx,
                    &bank,
                    transaction_status_sender.as_ref(),
                ) {
                    error!("Failed to accept scheduled commits: {:?}", err);
                } else {
                    // 2. Process those scheduled commits
                    // TODO: fix the possible delay here
                    // https://github.com/magicblock-labs/magicblock-validator/issues/104
                    if let Err(err) =
                        accounts_manager.process_scheduled_commits().await
                    {
                        error!(
                            "Failed to process scheduled commits: {:?}",
                            err
                        );
                    }
                }
            }
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
