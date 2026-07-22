use std::{ops::ControlFlow, time::Duration};

use magicblock_metrics::metrics;
use magicblock_rpc_client::{
    utils::{
        decide_rpc_error_flow, get_with_retries, map_magicblock_client_error,
        send_transaction_with_retries, SendErrorMapper, TransactionErrorMapper,
    },
    MagicBlockRpcClientError, MagicBlockRpcClientResult,
    MagicBlockSendTransactionConfig, MagicBlockSendTransactionOutcome,
    MagicblockRpcClient,
};
use solana_hash::Hash;
use solana_keypair::Keypair;
use solana_message::VersionedMessage;
use solana_rpc_client_api::config::RpcTransactionConfig;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_error::TransactionError;
use tracing::warn;

use crate::{
    intent_executor::{
        error::{IntentExecutorResult, InternalError},
        strategy_executor::error::{
            IntentTransactionErrorMapper, TransactionStrategyExecutionError,
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

    pub(in crate::intent_executor) async fn invalidate_cached_blockhash(&self) {
        self.rpc_client.invalidate_cached_blockhash().await;
    }

    pub(in crate::intent_executor) async fn execute_message_with_retries(
        &self,
        authority: &Keypair,
        prepared_message: VersionedMessage,
        tasks: &[BaseTaskImpl],
    ) -> IntentExecutorResult<Signature, TransactionStrategyExecutionError>
    {
        const RETRY_FOR: Duration = Duration::from_secs(2 * 60);
        const MIN_ATTEMPTS: usize = 3;

        // Send with retries
        let send_error_mapper = IntentErrorMapper {
            transaction_error_mapper: IntentTransactionErrorMapper { tasks },
            has_dedup_guard: tasks
                .iter()
                .any(|task| !matches!(task, BaseTaskImpl::BaseAction(_))),
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

    // Sends signed tx
    // Returns success on successfully sent tx, signature can be extracted from tx itself
    pub(in crate::intent_executor) async fn send_signed_tx_with_retries(
        &self,
        transaction: &VersionedTransaction,
        tasks: &[BaseTaskImpl],
    ) -> Result<(), TransactionStrategyExecutionError> {
        const RETRY_FOR: Duration = Duration::from_secs(2 * 60);
        const MIN_ATTEMPTS: usize = 3;

        // Send with retries
        let send_error_mapper = IntentErrorMapper {
            transaction_error_mapper: IntentTransactionErrorMapper { tasks },
            // Resending the identical already-signed transaction can't
            // double-apply actions - it either lands once or is rejected.
            has_dedup_guard: true,
        };
        let attempt = || async {
            self.rpc_client
                .send_transaction(
                    transaction,
                    &MagicBlockSendTransactionConfig::ensure_committed(),
                )
                .await
        };

        send_transaction_with_retries(
            attempt,
            send_error_mapper,
            |i, elapsed| !(elapsed < RETRY_FOR || i < MIN_ATTEMPTS),
        )
        .await?;
        Ok(())
    }

    /// Queries the full transaction history for the given signatures.
    /// Each entry is `None` if the signature was never included in a block.
    /// Use this for restart recovery where txs may be older than the RPC
    /// node's recent signature cache.
    pub(in crate::intent_executor) async fn get_signature_statuses_with_history(
        &self,
        signatures: &[Signature],
    ) -> MagicBlockRpcClientResult<Vec<Option<Result<(), TransactionError>>>>
    {
        let _timer = metrics::start_rpc_client_signature_history_timer();
        let response = self
            .rpc_client
            .get_inner()
            .get_signature_statuses_with_history(signatures)
            .await
            .map_err(MagicBlockRpcClientError::from)?;
        Ok(response
            .value
            .into_iter()
            .map(|s| s.map(|s| s.err.map_or(Ok(()), Err)))
            .collect())
    }

    /// Returns whether `blockhash` is still within its valid window, i.e.
    /// whether a transaction built with it could still land. Used to tell
    /// apart "not landed yet, may still land" from "guaranteed dead" when
    /// a pending signature isn't found on-chain.
    pub(in crate::intent_executor) async fn is_blockhash_valid(
        &self,
        blockhash: &Hash,
    ) -> MagicBlockRpcClientResult<bool> {
        self.rpc_client
            .get_inner()
            .is_blockhash_valid(blockhash, self.rpc_client.commitment())
            .await
            .map_err(MagicBlockRpcClientError::from)
    }

    pub(in crate::intent_executor) async fn get_latest_blockhash(
        &self,
    ) -> MagicBlockRpcClientResult<Hash> {
        const RETRY_FOR: Duration = Duration::from_secs(60);
        const MIN_ATTEMPTS: usize = 5;

        let attempt = || async { self.rpc_client.get_latest_blockhash().await };
        get_with_retries(attempt, DefaultGetErrorMapper, |i, elapsed| {
            !(elapsed < RETRY_FOR || i < MIN_ATTEMPTS)
        })
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

struct IntentErrorMapper<TxMap> {
    transaction_error_mapper: TxMap,
    /// Commit/finalize/undelegate tasks make a duplicate landing fail
    /// the whole retried transaction, so re-sends cannot double-apply
    has_dedup_guard: bool,
}

impl<TxMap> SendErrorMapper<InternalError> for IntentErrorMapper<TxMap>
where
    TxMap: TransactionErrorMapper<
        ExecutionError = TransactionStrategyExecutionError,
    >,
{
    type ExecutionError = TransactionStrategyExecutionError;
    fn map(&self, error: InternalError) -> Self::ExecutionError {
        if error.is_transaction_too_large() {
            TransactionStrategyExecutionError::TransactionTooLargeError(error)
        } else {
            match error {
                InternalError::MagicBlockRpcClientError(err) => {
                    map_magicblock_client_error(
                        &self.transaction_error_mapper,
                        *err,
                    )
                }
                err => TransactionStrategyExecutionError::InternalError(err),
            }
        }
    }

    fn decide_flow(
        &self,
        err: &Self::ExecutionError,
    ) -> ControlFlow<(), Duration> {
        let TransactionStrategyExecutionError::InternalError(
            InternalError::MagicBlockRpcClientError(err),
        ) = err
        else {
            return ControlFlow::Break(());
        };
        if self.has_dedup_guard {
            return decide_rpc_error_flow(err);
        }
        // Action-only transactions have no on-chain dedup: a re-send
        // with a fresh blockhash could execute the actions twice.
        // Only failures from before signing & sending are retriable.
        match err.as_ref() {
            MagicBlockRpcClientError::GetLatestBlockhash(_) => {
                decide_rpc_error_flow(err)
            }
            _ => ControlFlow::Break(()),
        }
    }
}

impl<TxMap> SendErrorMapper<MagicBlockRpcClientError>
    for IntentErrorMapper<TxMap>
where
    TxMap: TransactionErrorMapper<
        ExecutionError = TransactionStrategyExecutionError,
    >,
{
    type ExecutionError = TransactionStrategyExecutionError;
    fn map(&self, error: MagicBlockRpcClientError) -> Self::ExecutionError {
        if error.is_transaction_too_large() {
            TransactionStrategyExecutionError::TransactionTooLargeError(
                error.into(),
            )
        } else {
            map_magicblock_client_error(&self.transaction_error_mapper, error)
        }
    }

    fn decide_flow(
        &self,
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

struct DefaultGetErrorMapper;
impl SendErrorMapper<MagicBlockRpcClientError> for DefaultGetErrorMapper {
    type ExecutionError = MagicBlockRpcClientError;

    fn map(&self, error: MagicBlockRpcClientError) -> Self::ExecutionError {
        error
    }

    fn decide_flow(
        &self,
        mapped_error: &Self::ExecutionError,
    ) -> ControlFlow<(), Duration> {
        decide_rpc_error_flow(mapped_error)
    }
}
