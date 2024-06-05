use std::sync::Arc;

use crossbeam_channel::Receiver;
use itertools::izip;
use log::*;
use sleipnir_accounts_db::transaction_results::TransactionExecutionDetails;
use sleipnir_bank::transaction_notifier_interface::TransactionNotifierArc;
use sleipnir_geyser_plugin::{
    config::Config as GeyserPluginConfig, plugin::GrpcGeyserPlugin,
    rpc::GeyserRpcService,
};
use sleipnir_ledger::Ledger;
use sleipnir_transaction_status::{
    extract_and_fmt_memos, map_inner_instructions, TransactionStatusBatch,
    TransactionStatusMessage, TransactionStatusMeta,
};
use solana_geyser_plugin_manager::{
    geyser_plugin_manager::LoadedGeyserPlugin,
    geyser_plugin_service::{GeyserPluginService, GeyserPluginServiceError},
};

pub async fn init_geyser_service() -> Result<
    (GeyserPluginService, Arc<GeyserRpcService>),
    GeyserPluginServiceError,
> {
    let (cache_accounts, cache_transactions) =
        match std::env::var("GEYSER_CACHE_DISABLE") {
            Ok(val) => {
                let cache_accounts = !val.contains("accounts");
                let cache_transactions = !val.contains("transactions");
                (cache_accounts, cache_transactions)
            }
            Err(_) => (true, true),
        };
    let (enable_account_notifications, enable_transaction_notifications) =
        match std::env::var("GEYSER_DISABLE") {
            Ok(val) => {
                let enable_accounts = !val.contains("accounts");
                let enable_transactions = !val.contains("transactions");
                (enable_accounts, enable_transactions)
            }
            Err(_) => (true, true),
        };
    let config = GeyserPluginConfig {
        cache_accounts,
        cache_transactions,
        enable_account_notifications,
        enable_transaction_notifications,
        ..Default::default()
    };
    debug!("Geyser plugin config: {:?}", config);
    let (grpc_plugin, rpc_service) = {
        let plugin = GrpcGeyserPlugin::create(config)
            .await
            .map_err(|err| {
                error!("Failed to load geyser plugin: {:?}", err);
                err
            })
            .expect("Failed to load grpc geyser plugin");
        let rpc_service = plugin.rpc();
        (LoadedGeyserPlugin::new(Box::new(plugin), None), rpc_service)
    };
    let geyser_service = GeyserPluginService::new(&[], vec![grpc_plugin])?;
    Ok((geyser_service, rpc_service))
}

pub struct GeyserTransactionNotifyListener {
    transaction_notifier: Option<TransactionNotifierArc>,
    transaction_recvr: Receiver<TransactionStatusMessage>,
    ledger: Arc<Ledger>,
}

impl GeyserTransactionNotifyListener {
    pub fn new(
        transaction_notifier: Option<TransactionNotifierArc>,
        transaction_recvr: Receiver<TransactionStatusMessage>,
        ledger: Arc<Ledger>,
    ) -> Self {
        Self {
            transaction_notifier,
            transaction_recvr,
            ledger,
        }
    }

    pub fn run(&self, enable_rpc_transaction_history: bool) {
        let transaction_notifier = match self.transaction_notifier {
            Some(ref notifier) => notifier.clone(),
            None => return,
        };
        let transaction_recvr = self.transaction_recvr.clone();
        let ledger = self.ledger.clone();
        std::thread::spawn(move || {
            while let Ok(message) = transaction_recvr.recv() {
                // Mostly from: rpc/src/transaction_status_service.rs
                match message {
                    TransactionStatusMessage::Batch(
                        TransactionStatusBatch {
                            bank,
                            transactions,
                            execution_results,
                            balances,
                            token_balances,
                            transaction_indexes,
                            ..
                        },
                    ) => {
                        let slot = bank.slot();
                        for (
                            transaction,
                            execution_result,
                            pre_balances,
                            post_balances,
                            pre_token_balances,
                            post_token_balances,
                            transaction_index,
                        ) in izip!(
                            transactions,
                            execution_results,
                            balances.pre_balances,
                            balances.post_balances,
                            token_balances.pre_token_balances,
                            token_balances.post_token_balances,
                            transaction_indexes,
                        ) {
                            if let Some(details) = execution_result {
                                let TransactionExecutionDetails {
                                    status,
                                    log_messages,
                                    inner_instructions,
                                    return_data,
                                    executed_units,
                                    ..
                                } = details;
                                let lamports_per_signature =
                                    bank.get_lamports_per_signature();
                                let fee = bank.get_fee_for_message_with_lamports_per_signature(
                                    transaction.message(),
                                    lamports_per_signature,
                                );
                                let inner_instructions = inner_instructions
                                    .map(|inner_instructions| {
                                        map_inner_instructions(
                                            inner_instructions,
                                        )
                                        .collect()
                                    });
                                let pre_token_balances =
                                    Some(pre_token_balances);
                                let post_token_balances =
                                    Some(post_token_balances);
                                // NOTE: we don't charge rent and rewards are based on rent_debits
                                let rewards = None;
                                let loaded_addresses =
                                    transaction.get_loaded_addresses();
                                let transaction_status_meta =
                                    TransactionStatusMeta {
                                        status,
                                        fee,
                                        pre_balances,
                                        post_balances,
                                        inner_instructions,
                                        log_messages,
                                        pre_token_balances,
                                        post_token_balances,
                                        rewards,
                                        loaded_addresses,
                                        return_data,
                                        compute_units_consumed: Some(
                                            executed_units,
                                        ),
                                    };

                                transaction_notifier.notify_transaction(
                                    slot,
                                    transaction_index,
                                    transaction.signature(),
                                    &transaction_status_meta,
                                    &transaction,
                                );
                                if enable_rpc_transaction_history {
                                    if let Some(memos) = extract_and_fmt_memos(
                                        transaction.message(),
                                    ) {
                                        ledger
                                            .write_transaction_memos(transaction.signature(), slot, memos)
                                            .expect("Expect database write to succeed: TransactionMemos");
                                    }
                                    ledger.write_transaction(
                                        *transaction.signature(),
                                        slot,
                                        transaction,
                                        transaction_status_meta,
                                        transaction_index,
                                    )
                                    .expect("Expect database write to succeed: TransactionStatus");
                                }
                            }
                        }
                    }
                    TransactionStatusMessage::Freeze(_slot) => {}
                }
            }
        });
    }
}
