use std::{ops::ControlFlow, time::Duration};

use delegation_program_api::pda::commit_record_pda_from_delegated_account;
use metrics::metrics;
use rpc_client::{
    MagicBlockRpcClientError, MagicBlockRpcClientResult, MagicBlockSendTransactionConfig,
    MagicBlockSendTransactionOutcome, MagicblockRpcClient,
    utils::{
        SendErrorMapper, TransactionErrorMapper, decide_rpc_error_flow, get_with_retries,
        map_magicblock_client_error, send_transaction_with_retries,
    },
};
use solana_commitment_config::CommitmentConfig;
use solana_hash::Hash;
use solana_keypair::Keypair;
use solana_message::VersionedMessage;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::config::RpcTransactionConfig;
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_error::TransactionError;
use tracing::warn;

use crate::{
    intent_executor::{
        ExecutionOutput,
        error::{IntentExecutorError, IntentExecutorResult, InternalError},
        strategy_executor::error::{
            IntentTransactionErrorMapper, TransactionStrategyExecutionError,
        },
    },
    tasks::{BaseTaskImpl, utils::TransactionUtils},
    transaction_preparator::error::TransactionPreparatorError,
};

#[derive(Clone)]
pub struct IntentExecutionClient {
    rpc_client: MagicblockRpcClient,
    compute_unit_price: u64,
}

impl IntentExecutionClient {
    pub fn new(rpc_client: MagicblockRpcClient, compute_unit_price: u64) -> Self {
        IntentExecutionClient { rpc_client, compute_unit_price }
    }

    // Temporary compatibility for commit-only transactions; remove with #1801.
    /// Repairs a pre-upgrade commit without restoring the removed Finalize task.
    /// Retry the same signed transaction, never a fresh signature after an uncertain send.
    pub(in crate::intent_executor) async fn finalize_account(
        &self,
        authority: &Keypair,
        account: Pubkey,
    ) -> Result<Result<Signature, TransactionStrategyExecutionError>, TransactionPreparatorError>
    {
        let instruction =
            delegation_program_api::instruction_builder::finalize(authority.pubkey(), account);
        let budgets = TransactionUtils::budget_instructions(120_000, self.compute_unit_price, 0);
        let mut transaction =
            TransactionUtils::assemble_tx_raw(authority, &[instruction], &budgets, &[])?;
        let blockhash = self
            .get_latest_blockhash()
            .await
            .map_err(TransactionPreparatorError::GetLatestBlockhashError)?;
        transaction.message.set_recent_blockhash(blockhash);
        let transaction = VersionedTransaction::try_new(transaction.message, &[authority])?;
        Ok(self
            .send_signed_tx_with_retries(&transaction, &[])
            .await
            .map(|()| transaction.signatures[0]))
    }

    /// On resumed two-stage intents, finalize only accounts with a remaining DLP commit record.
    pub(in crate::intent_executor) async fn finalize_legacy_commits(
        &self,
        authority: &Keypair,
        accounts: &[Pubkey],
    ) -> IntentExecutorResult<()> {
        if accounts.is_empty() {
            return Ok(());
        }
        let records: Vec<_> =
            accounts.iter().map(commit_record_pda_from_delegated_account).collect();
        let records =
            self.rpc_client
                .get_inner()
                .get_multiple_accounts(&records)
                .await
                .map_err(|err| {
                    IntentExecutorError::FailedExecution(MagicBlockRpcClientError::from(err).into())
                })?;
        for (&pubkey, record) in accounts.iter().zip(records) {
            if record.is_none_or(|record| record.owner != delegation_program_api::id()) {
                continue;
            }
            self.finalize_account(authority, pubkey)
                .await
                .map_err(IntentExecutorError::FailedPreparation)?
                .map_err(IntentExecutorError::FailedExecution)?;
        }
        Ok(())
    }

    pub(in crate::intent_executor) async fn invalidate_cached_blockhash(&self) {
        self.rpc_client.invalidate_cached_blockhash().await;
    }

    pub(in crate::intent_executor) async fn execute_message_with_retries(
        &self,
        authority: &Keypair,
        prepared_message: VersionedMessage,
        tasks: &[BaseTaskImpl],
    ) -> IntentExecutorResult<Signature, TransactionStrategyExecutionError> {
        const RETRY_FOR: Duration = Duration::from_secs(2 * 60);
        const MIN_ATTEMPTS: usize = 3;

        // Send with retries
        let send_error_mapper = IntentErrorMapper {
            transaction_error_mapper: IntentTransactionErrorMapper {
                tasks,
                task_instruction_offset: if matches!(prepared_message, VersionedMessage::V1(_)) {
                    0
                } else {
                    TransactionUtils::COMPUTE_BUDGET_INSTRUCTION_COUNT
                },
            },
            has_dedup_guard: tasks.iter().any(|task| !matches!(task, BaseTaskImpl::BaseAction(_))),
        };
        let attempt =
            || async { self.send_prepared_message(authority, prepared_message.clone()).await };
        send_transaction_with_retries(attempt, send_error_mapper, |i, elapsed| {
            !(elapsed < RETRY_FOR || i < MIN_ATTEMPTS)
        })
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
            transaction_error_mapper: IntentTransactionErrorMapper {
                tasks,
                task_instruction_offset: if matches!(transaction.message, VersionedMessage::V1(_)) {
                    0
                } else {
                    TransactionUtils::COMPUTE_BUDGET_INSTRUCTION_COUNT
                },
            },
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

        send_transaction_with_retries(attempt, send_error_mapper, |i, elapsed| {
            !(elapsed < RETRY_FOR || i < MIN_ATTEMPTS)
        })
        .await?;
        Ok(())
    }

    /// Queries the full transaction history for the given signatures and
    /// filters each result down to `commitment_config`.
    /// Each entry is `None` if the signature was never included in a block,
    /// or if it landed but hasn't yet reached the configured commitment
    /// level - callers must not treat such an entry as final, since a
    /// merely-processed transaction can still be dropped by a fork.
    /// Use this for restart recovery where txs may be older than the RPC
    /// node's recent signature cache.
    pub(in crate::intent_executor) async fn get_signature_statuses_with_history_at_commitment(
        &self,
        signatures: &[Signature],
        commitment_config: CommitmentConfig,
    ) -> MagicBlockRpcClientResult<Vec<Option<Result<(), TransactionError>>>> {
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
            .map(|status| {
                status.and_then(|status| {
                    status
                        .satisfies_commitment(commitment_config)
                        .then(|| status.err.map_or(Ok(()), Err))
                })
            })
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
    ) -> IntentExecutorResult<MagicBlockSendTransactionOutcome, InternalError> {
        let latest_blockhash = self.rpc_client.get_latest_blockhash().await?;
        match &mut prepared_message {
            VersionedMessage::V0(value) => {
                value.recent_blockhash = latest_blockhash;
            }
            VersionedMessage::Legacy(value) => {
                warn!("Legacy message not expected");
                value.recent_blockhash = latest_blockhash;
            }
            VersionedMessage::V1(value) => {
                warn!("V1 message not expected");
                // V1 renames the blockhash to the lifetime specifier.
                value.lifetime_specifier = latest_blockhash;
            }
        };

        let transaction = VersionedTransaction::try_new(prepared_message, &[&authority])?;
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
            max_supported_transaction_version: Some(1),
            ..Default::default()
        };
        let cu_metrics = || async {
            let mut total = 0;
            for signature in execution_outcome.progress().signatures(None) {
                let tx = self.rpc_client.get_transaction(&signature, Some(config)).await?;
                let Some(cu) = extract_cu(tx.transaction) else { return Ok(None) };
                total += cu;
            }
            Ok::<_, MagicBlockRpcClientError>(Some(total))
        };

        match cu_metrics().await {
            Ok(Some(cu)) => {
                metrics::set_commmittor_intent_cu_usage(i64::try_from(cu).unwrap_or(i64::MAX))
            }
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
    TxMap: TransactionErrorMapper<ExecutionError = TransactionStrategyExecutionError>,
{
    type ExecutionError = TransactionStrategyExecutionError;
    fn map(&self, error: InternalError) -> Self::ExecutionError {
        if error.is_transaction_too_large() {
            TransactionStrategyExecutionError::TransactionTooLargeError(error)
        } else {
            match error {
                InternalError::MagicBlockRpcClientError(err) => {
                    map_magicblock_client_error(&self.transaction_error_mapper, *err)
                }
                err => TransactionStrategyExecutionError::InternalError(err),
            }
        }
    }

    fn decide_flow(&self, err: &Self::ExecutionError) -> ControlFlow<(), Duration> {
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
            MagicBlockRpcClientError::GetLatestBlockhash(_) => decide_rpc_error_flow(err),
            _ => ControlFlow::Break(()),
        }
    }
}

impl<TxMap> SendErrorMapper<MagicBlockRpcClientError> for IntentErrorMapper<TxMap>
where
    TxMap: TransactionErrorMapper<ExecutionError = TransactionStrategyExecutionError>,
{
    type ExecutionError = TransactionStrategyExecutionError;
    fn map(&self, error: MagicBlockRpcClientError) -> Self::ExecutionError {
        if error.is_transaction_too_large() {
            TransactionStrategyExecutionError::TransactionTooLargeError(error.into())
        } else {
            map_magicblock_client_error(&self.transaction_error_mapper, error)
        }
    }

    fn decide_flow(&self, err: &Self::ExecutionError) -> ControlFlow<(), Duration> {
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

    fn decide_flow(&self, mapped_error: &Self::ExecutionError) -> ControlFlow<(), Duration> {
        decide_rpc_error_flow(mapped_error)
    }
}
