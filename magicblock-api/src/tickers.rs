use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use log::*;
use magicblock_accounts::AccountsManager;
use magicblock_bank::bank::Bank;
use magicblock_core::magic_program;
use magicblock_ledger::Ledger;
use magicblock_metrics::metrics;
use magicblock_processor::execute_transaction::execute_legacy_transaction;
use magicblock_program::{
    magicblock_instruction::accept_scheduled_commits, MagicContext,
};
use magicblock_transaction_status::TransactionStatusSender;
use solana_sdk::account::ReadableAccount;
use tokio_util::sync::CancellationToken;

use crate::slot::advance_slot_and_update_ledger;

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

            let (update_ledger_result, next_slot) =
                advance_slot_and_update_ledger(&bank, &ledger);
            if let Err(err) = update_ledger_result {
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
    fn try_set_ledger_counts(ledger: &Ledger) {
        macro_rules! try_set_ledger_count {
            ($name:ident) => {
                paste::paste! {
                    match ledger.[< count_ $name >]() {
                        Ok(count) => {
                            metrics::[< set_ledger_ $name _count >](count);
                        }
                        Err(err) => warn!(
                            "Failed to get ledger {} count: {:?}",
                            stringify!($name),
                            err
                        ),
                    }
                }
            };
        }
        try_set_ledger_count!(block_times);
        try_set_ledger_count!(blockhashes);
        try_set_ledger_count!(slot_signatures);
        try_set_ledger_count!(address_signatures);
        try_set_ledger_count!(transaction_status);
        try_set_ledger_count!(transaction_successful_status);
        try_set_ledger_count!(transaction_failed_status);
        try_set_ledger_count!(transactions);
        try_set_ledger_count!(transaction_memos);
        try_set_ledger_count!(perf_samples);
        try_set_ledger_count!(account_mod_data);
    }

    fn try_set_ledger_storage_size(ledger: &Ledger) {
        match ledger.storage_size() {
            Ok(byte_size) => metrics::set_ledger_size(byte_size),
            Err(err) => warn!("Failed to get ledger storage size: {:?}", err),
        }
    }
    fn try_set_accounts_storage_size(bank: &Bank) {
        match bank.accounts_db_storage_size() {
            Ok(byte_size) => metrics::set_accounts_size(byte_size),
            Err(err) => warn!("Failed to get accounts storage size: {:?}", err),
        }
    }
    let ledger = ledger.clone();
    let bank = bank.clone();
    try_set_ledger_storage_size(&ledger);
    try_set_accounts_storage_size(&bank);
    try_set_ledger_counts(&ledger);

    tokio::task::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(tick_duration) => {
                    try_set_ledger_storage_size(&ledger);
                    try_set_accounts_storage_size(&bank);
                    try_set_ledger_counts(&ledger);
                },
                _ = token.cancelled() => {
                    break;
                }
            }
        }
    })
}
