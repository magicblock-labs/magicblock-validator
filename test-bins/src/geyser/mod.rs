use crossbeam_channel::Receiver;
use itertools::izip;
use sleipnir_bank::transaction_notifier_interface::TransactionNotifierArc;
use sleipnir_geyser_plugin::plugin::GrpcGeyserPlugin;
use sleipnir_transaction_status::{
    map_inner_instructions, TransactionStatusBatch, TransactionStatusMessage,
    TransactionStatusMeta,
};
use solana_accounts_db::transaction_results::TransactionExecutionDetails;
use solana_geyser_plugin_manager::{
    geyser_plugin_manager::LoadedGeyserPlugin,
    geyser_plugin_service::{GeyserPluginService, GeyserPluginServiceError},
};

pub fn init_geyser_service(
) -> Result<GeyserPluginService, GeyserPluginServiceError> {
    let grpc_plugin = LoadedGeyserPlugin::new(
        Box::new(GrpcGeyserPlugin::new(Default::default())),
        None,
    );
    let geyser_service = GeyserPluginService::new(&[], vec![grpc_plugin])?;

    Ok(geyser_service)
}

pub struct GeyserTransactionNotifyListener {
    transaction_notifier: TransactionNotifierArc,
    transaction_recvr: Receiver<TransactionStatusMessage>,
}

impl GeyserTransactionNotifyListener {
    pub fn new(
        transaction_notifier: TransactionNotifierArc,
        transaction_recvr: Receiver<TransactionStatusMessage>,
    ) -> Self {
        Self {
            transaction_notifier,
            transaction_recvr,
        }
    }

    pub fn run(&self) {
        let transaction_notifier = self.transaction_notifier.clone();
        let transaction_recvr = self.transaction_recvr.clone();
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
                            }
                        }
                    }
                    TransactionStatusMessage::Freeze(_slot) => {}
                }
            }
        });
    }
}
