use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use log::*;
use magicblock_accounts::ScheduledCommitsProcessor;
use magicblock_accounts_db::AccountsDb;
use magicblock_core::{
    link::{blocks::BlockUpdateTx, transactions::TransactionSchedulerHandle},
    traits::AccountsBank,
};
use magicblock_ledger::{LatestBlock, Ledger};
use magicblock_magic_program_api as magic_program;
use magicblock_metrics::metrics;
use magicblock_program::{instruction_utils::InstructionUtils, MagicContext};
use solana_sdk::account::ReadableAccount;
use tokio_util::sync::CancellationToken;

use crate::slot::advance_slot_and_update_ledger;

pub fn init_slot_ticker<C: ScheduledCommitsProcessor>(
    accountsdb: Arc<AccountsDb>,
    committor_processor: &Option<Arc<C>>,
    ledger: Arc<Ledger>,
    tick_duration: Duration,
    transaction_scheduler: TransactionSchedulerHandle,
    block_updates_tx: BlockUpdateTx,
    exit: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    let committor_processor = committor_processor.clone();

    let latest_block = ledger.latest_block().clone();
    tokio::task::spawn(async move {
        let log = tick_duration >= Duration::from_secs(5);
        while !exit.load(Ordering::Relaxed) {
            tokio::time::sleep(tick_duration).await;

            let (update_ledger_result, next_slot) =
                advance_slot_and_update_ledger(
                    &accountsdb,
                    &ledger,
                    &block_updates_tx,
                );
            if let Err(err) = update_ledger_result {
                error!("Failed to write block: {:?}", err);
            }

            if log {
                debug!("Advanced to slot {}", next_slot);
            }
            metrics::inc_slot();

            // Handle intents if such feature enabled
            let Some(committor_processor) = &committor_processor else {
                continue;
            };

            // If accounts were scheduled to be committed, we accept them here
            // and processs the commits
            let magic_context_acc = accountsdb.get_account(&magic_program::MAGIC_CONTEXT_PUBKEY)
                .expect("Validator found to be running without MagicContext account!");
            if MagicContext::has_scheduled_commits(magic_context_acc.data()) {
                handle_scheduled_commits(
                    committor_processor,
                    &transaction_scheduler,
                    &latest_block,
                )
                .await;
            }
            if log {
                debug!("Advanced to slot {}", next_slot);
            }
        }
        metrics::inc_slot();
    })
}

async fn handle_scheduled_commits<C: ScheduledCommitsProcessor>(
    committor_processor: &Arc<C>,
    transaction_scheduler: &TransactionSchedulerHandle,
    latest_block: &LatestBlock,
) {
    // 1. Send the transaction to move the scheduled commits from the MagicContext
    //    to the global ScheduledCommit store
    let tx = InstructionUtils::accept_scheduled_commits(
        latest_block.load().blockhash,
    );
    if let Err(err) = transaction_scheduler.execute(tx).await {
        error!("Failed to accept scheduled commits: {:?}", err);
        return;
    }

    // 2. Process those scheduled commits
    // TODO: fix the possible delay here
    // https://github.com/magicblock-labs/magicblock-validator/issues/104
    if let Err(err) = committor_processor.process().await {
        error!("Failed to process scheduled commits: {:?}", err);
    }
}

#[allow(unused_variables)]
pub fn init_system_metrics_ticker(
    tick_duration: Duration,
    ledger: &Arc<Ledger>,
    accountsdb: &Arc<AccountsDb>,
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
    fn set_accounts_storage_size(accounts_db: &AccountsDb) {
        let byte_size = accounts_db.storage_size();
        metrics::set_accounts_size(byte_size.try_into().unwrap_or(i64::MAX));
    }
    fn set_accounts_count(accounts_db: &AccountsDb) {
        metrics::set_accounts_count(
            accounts_db
                .get_accounts_count()
                .try_into()
                .unwrap_or(i64::MAX),
        );
    }

    let ledger = ledger.clone();
    let bank = accountsdb.clone();
    tokio::task::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(tick_duration) => {
                    try_set_ledger_storage_size(&ledger);
                    set_accounts_storage_size(&bank);
                    metrics::observe_columns_count_duration(|| try_set_ledger_counts(&ledger));
                    set_accounts_count(&bank);
                },
                _ = token.cancelled() => {
                    break;
                }
            }
        }
    })
}
