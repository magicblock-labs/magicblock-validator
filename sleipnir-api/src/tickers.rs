use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use log::*;
use sleipnir_accounts::AccountsManager;
use sleipnir_accounts_db::FLUSH_ACCOUNTS_SLOT_FREQ;
use sleipnir_bank::bank::Bank;
use sleipnir_core::magic_program;
use sleipnir_ledger::Ledger;
use sleipnir_metrics::metrics;
use sleipnir_processor::execute_transaction::{
    execute_legacy_transaction, lock_transactions,
};
use sleipnir_program::{
    sleipnir_instruction::accept_scheduled_commits, MagicContext,
};
use sleipnir_transaction_status::TransactionStatusSender;
use solana_sdk::account::ReadableAccount;
use tokio_util::sync::CancellationToken;

use crate::accounts::flush_accounts;

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

            let prev_slot = bank.slot();

            let next_slot = if prev_slot % FLUSH_ACCOUNTS_SLOT_FREQ == 0 {
                // NOTE: at this point we flush the accounts blocking the slot from advancing as
                // well as holding the transaction lock.
                // This is done on purpose in order to avoid transactions writing to the accounts
                // while we are persisting them.
                // This is a very slow operation, i.e. in the 30ms+ range and we should consider
                // making a copy of all accounts, including data and then performing the IO flush
                // in a separate task.
                // Also in this case we prevent the transactions from advancing before the bank
                // slot advanced since only then can we be sure that the accounts did not change
                // during the same slot after we flushed them.
                let _lock = lock_transactions();
                flush_accounts(&bank);
                bank.advance_slot()
            } else {
                bank.advance_slot()
            };

            // Update ledger with previous block's metas
            if let Err(err) = ledger.write_block(
                prev_slot,
                timestamp_in_secs() as i64,
                bank.last_blockhash(),
            ) {
                error!("Failed to write block: {:?}", err);
            }

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
                info!("Advanced to slot {}", next_slot);
            }
            metrics::inc_slot();
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

pub fn init_system_metrics_ticker(
    tick_duration: Duration,
    ledger: &Arc<Ledger>,
    bank: &Arc<Bank>,
    token: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    fn try_set_ledger_storage_size(ledger: &Ledger) {
        match ledger.storage_size() {
            Ok(byte_size) => metrics::set_ledger_size(byte_size),
            Err(err) => warn!("Failed to get ledger storage size: {:?}", err),
        }
    }
    fn try_set_accounts_storage_size(bank: &Bank) {
        match bank.accounts_db().storage_size() {
            Ok(byte_size) => metrics::set_accounts_size(byte_size),
            Err(err) => warn!("Failed to get accounts storage size: {:?}", err),
        }
    }
    let ledger = ledger.clone();
    let bank = bank.clone();
    try_set_ledger_storage_size(&ledger);
    try_set_accounts_storage_size(&bank);
    tokio::task::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(tick_duration) => {
                    try_set_ledger_storage_size(&ledger);
                    try_set_accounts_storage_size(&bank);
                },
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
