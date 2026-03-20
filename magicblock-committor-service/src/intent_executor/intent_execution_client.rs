use std::{ops::ControlFlow, time::Duration};

use magicblock_metrics::metrics;
use magicblock_rpc_client::{
    utils::{
        decide_rpc_error_flow, map_magicblock_client_error,
        send_transaction_with_retries, SendErrorMapper, TransactionErrorMapper,
    },
    MagicBlockRpcClientError, MagicBlockSendTransactionConfig,
    MagicBlockSendTransactionOutcome, MagicblockRpcClient,
};
use solana_keypair::Keypair;
use solana_message::VersionedMessage;
use solana_rpc_client_api::config::RpcTransactionConfig;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use tracing::warn;

use crate::{
    intent_executor::{
        error::{
            IntentExecutorResult, IntentTransactionErrorMapper, InternalError,
            TransactionStrategyExecutionError,
        },
        ExecutionOutput,
    },
    tasks::BaseTaskImpl,
};

#[derive(Clone)]
pub struct IntentExecutionClient {
    rpc_client: MagicblockRpcClient,
}

impl IntentExecutionClient {
    pub fn new(rpc_client: MagicblockRpcClient) -> Self {
        IntentExecutionClient { rpc_client }
    }

    pub(in crate::intent_executor) async fn execute_message_with_retries(
        &self,
        authority: &Keypair,
        prepared_message: VersionedMessage,
        tasks: &[BaseTaskImpl],
    ) -> IntentExecutorResult<Signature, TransactionStrategyExecutionError>
    {
        struct IntentErrorMapper<TxMap> {
            transaction_error_mapper: TxMap,
        }
        impl<TxMap> SendErrorMapper<InternalError> for IntentErrorMapper<TxMap>
        where
            TxMap: TransactionErrorMapper<
                ExecutionError = TransactionStrategyExecutionError,
            >,
        {
            type ExecutionError = TransactionStrategyExecutionError;
            fn map(&self, error: InternalError) -> Self::ExecutionError {
                match error {
                    InternalError::MagicBlockRpcClientError(err) => {
                        map_magicblock_client_error(
                            &self.transaction_error_mapper,
                            *err,
                        )
                    }
                    err => {
                        TransactionStrategyExecutionError::InternalError(err)
                    }
                }
            }

            fn decide_flow(
                err: &Self::ExecutionError,
            ) -> ControlFlow<(), Duration> {
                match err {
                    TransactionStrategyExecutionError::InternalError(
                        InternalError::MagicBlockRpcClientError(err),
                    ) => decide_rpc_error_flow(err),
                    _ => ControlFlow::Break(()),
                }
            }
        }

        const RETRY_FOR: Duration = Duration::from_secs(2 * 60);
        const MIN_ATTEMPTS: usize = 3;

        // Send with retries
        let send_error_mapper = IntentErrorMapper {
            transaction_error_mapper: IntentTransactionErrorMapper { tasks },
        };
        let attempt = || async {
            self.send_prepared_message(authority, prepared_message.clone())
                .await
        };
        send_transaction_with_retries(
            attempt,
            send_error_mapper,
            |i, elapsed| !(elapsed < RETRY_FOR || i < MIN_ATTEMPTS),
        )
        .await
    }

    /// Shared helper for sending transactions
    async fn send_prepared_message(
        &self,
        authority: &Keypair,
        mut prepared_message: VersionedMessage,
    ) -> IntentExecutorResult<MagicBlockSendTransactionOutcome, InternalError>
    {
        let latest_blockhash = self.rpc_client.get_latest_blockhash().await?;
        match &mut prepared_message {
            VersionedMessage::V0(value) => {
                value.recent_blockhash = latest_blockhash;
            }
            VersionedMessage::Legacy(value) => {
                warn!("Legacy message not expected");
                value.recent_blockhash = latest_blockhash;
            }
        };

        let transaction =
            VersionedTransaction::try_new(prepared_message, &[&authority])?;
        let result = self
            .rpc_client
            .send_transaction(
                &transaction,
                &MagicBlockSendTransactionConfig::ensure_committed(),
            )
            .await?;

        Ok(result)
    }

    pub async fn intent_metrics(&self, execution_outcome: ExecutionOutput) {
        use solana_transaction_status_client_types::EncodedTransactionWithStatusMeta;
        fn extract_cu(tx: EncodedTransactionWithStatusMeta) -> Option<u64> {
            let cu = tx.meta?.compute_units_consumed;
            cu.into()
        }

        let config = RpcTransactionConfig {
            commitment: Some(self.rpc_client.commitment()),
            max_supported_transaction_version: Some(0),
            ..Default::default()
        };
        let cu_metrics = || async {
            match execution_outcome {
                ExecutionOutput::SingleStage(signature) => {
                    let tx = self
                        .rpc_client
                        .get_transaction(&signature, Some(config))
                        .await?;
                    Ok::<_, MagicBlockRpcClientError>(extract_cu(
                        tx.transaction,
                    ))
                }
                ExecutionOutput::TwoStage {
                    commit_signature,
                    finalize_signature,
                } => {
                    let commit_tx = self
                        .rpc_client
                        .get_transaction(&commit_signature, Some(config))
                        .await?;
                    let finalize_tx = self
                        .rpc_client
                        .get_transaction(&finalize_signature, Some(config))
                        .await?;
                    let commit_cu = extract_cu(commit_tx.transaction);
                    let finalize_cu = extract_cu(finalize_tx.transaction);
                    let (Some(commit_cu), Some(finalize_cu)) =
                        (commit_cu, finalize_cu)
                    else {
                        return Ok(None);
                    };
                    Ok(Some(commit_cu + finalize_cu))
                }
            }
        };

        match cu_metrics().await {
            Ok(Some(cu)) => metrics::set_commmittor_intent_cu_usage(
                i64::try_from(cu).unwrap_or(i64::MAX),
            ),
            Err(err) => warn!(error = ?err, "Failed to fetch CUs for intent"),
            _ => {}
        }
    }
}
