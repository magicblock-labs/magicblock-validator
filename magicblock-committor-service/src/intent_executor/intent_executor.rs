use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt;
use log::{debug, error, warn};
use magicblock_program::{
    magic_scheduled_base_intent::ScheduledBaseIntent,
    validator::validator_authority,
};
use magicblock_rpc_client::{
    MagicBlockRpcClientError, MagicBlockSendTransactionConfig,
    MagicBlockSendTransactionOutcome, MagicblockRpcClient,
};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::client_error::ErrorKind;
use solana_sdk::{
    instruction::InstructionError,
    message::VersionedMessage,
    signature::{Keypair, Signature},
    signer::{Signer, SignerError},
    transaction::{TransactionError, VersionedTransaction},
};
use tokio::time::{sleep, Instant};

use crate::{
    intent_executor::{
        error::{
            IntentExecutorError, IntentExecutorResult, InternalError,
            TransactionStrategyExecutionError,
        },
        task_info_fetcher::TaskInfoFetcher,
        ExecutionOutput, IntentExecutor,
    },
    persist::{CommitStatus, CommitStatusSignatures, IntentPersister},
    tasks::{
        task_builder::{TaskBuilderV1, TasksBuilder},
        task_strategist::{
            TaskStrategist, TaskStrategistError, TransactionStrategy,
        },
        tasks::{BaseTask, TaskType},
    },
    transaction_preparator::{
        error::TransactionPreparatorError,
        transaction_preparator::TransactionPreparator,
    },
    utils::persist_status_update_by_message_set,
};

pub struct IntentExecutorImpl<T, F> {
    authority: Keypair,
    rpc_client: MagicblockRpcClient,
    transaction_preparator: T,
    task_info_fetcher: Arc<F>,
}

impl<T, F> IntentExecutorImpl<T, F>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
{
    pub fn new(
        rpc_client: MagicblockRpcClient,
        transaction_preparator: T,
        task_info_fetcher: Arc<F>,
    ) -> Self {
        let authority = validator_authority();
        Self {
            authority,
            rpc_client,
            transaction_preparator,
            task_info_fetcher,
        }
    }

    /// Checks if it is possible to unite Commit & Finalize stages in 1 transaction
    /// Returns corresponding `TransactionStrategy` if possible, otherwise `None`
    fn try_unite_tasks<P: IntentPersister>(
        commit_tasks: &[Box<dyn BaseTask>],
        finalize_task: &[Box<dyn BaseTask>],
        authority: &Pubkey,
        persister: &Option<P>,
    ) -> Result<Option<TransactionStrategy>, SignerError> {
        const MAX_UNITED_TASKS_LEN: usize = 22;

        // We can unite in 1 tx a lot of commits
        // but then there's a possibility of hitting CPI limit, aka
        // MaxInstructionTraceLengthExceeded error.
        // So we limit tasks len with 22 total tasks
        // In case this fails as well, it will be retried with TwoStage approach
        // on retry, once retries are introduced
        if commit_tasks.len() + finalize_task.len() > MAX_UNITED_TASKS_LEN {
            return Ok(None);
        }

        // Clone tasks since strategies applied to united case maybe suboptimal for regular one
        let mut commit_tasks = commit_tasks.to_owned();
        let finalize_task = finalize_task.to_owned();

        // Unite tasks to attempt running as single tx
        commit_tasks.extend(finalize_task);
        match TaskStrategist::build_strategy(commit_tasks, authority, persister)
        {
            Ok(strategy) => Ok(Some(strategy)),
            Err(TaskStrategistError::FailedToFitError) => Ok(None),
            Err(TaskStrategistError::SignerError(err)) => Err(err),
        }
    }

    async fn execute_inner<P: IntentPersister>(
        &self,
        base_intent: ScheduledBaseIntent,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        if base_intent.is_empty() {
            return Err(IntentExecutorError::EmptyIntentError);
        }

        // Update tasks status to Pending
        if let Some(pubkeys) = base_intent.get_committed_pubkeys() {
            let update_status = CommitStatus::Pending;
            persist_status_update_by_message_set(
                persister,
                base_intent.id,
                &pubkeys,
                update_status,
            );
        }

        // Build tasks for Commit & Finalize stages
        let commit_tasks = TaskBuilderV1::commit_tasks(
            &self.task_info_fetcher,
            &base_intent,
            persister,
        )
        .await?;
        let finalize_tasks = TaskBuilderV1::finalize_tasks(
            &self.task_info_fetcher,
            &base_intent,
        )
        .await?;

        // See if we can squeeze them in one tx
        if let Some(single_tx_strategy) = Self::try_unite_tasks(
            &commit_tasks,
            &finalize_tasks,
            &self.authority.pubkey(),
            persister,
        )? {
            debug!("Executing intent in single stage");
            let output = self
                .execute_single_stage(single_tx_strategy, persister)
                .await?;

            Ok(output)
        } else {
            debug!("Executing intent in two stages");
            // Build strategy for Commit stage
            let commit_strategy = TaskStrategist::build_strategy(
                commit_tasks,
                &self.authority.pubkey(),
                persister,
            )?;

            // Build strategy for Finalize stage
            let finalize_strategy = TaskStrategist::build_strategy(
                finalize_tasks,
                &self.authority.pubkey(),
                persister,
            )?;

            // TODO: move and handle retries within those/
            let output = self
                .execute_two_stages(
                    commit_strategy,
                    finalize_strategy,
                    persister,
                )
                .await?;

            // Cleanup
            {
                let authority_copy = self.authority.insecure_clone();
                tokio::spawn(async move {
                    self.transaction_preparator
                        .cleanup_for_strategy(&authority_copy, commit_strategy)
                        .await;
                });
            }

            {
                let authority_copy = self.authority.insecure_clone();
                tokio::spawn(async move {
                    self.transaction_preparator
                        .cleanup_for_strategy(
                            &authority_copy,
                            finalize_strategy,
                        )
                        .await;
                });
            }

            Ok(output)
        }
    }

    /// Starting execution from single stage
    async fn single_stage_execution_flow<P: IntentPersister>(
        &self,
        transaction_strategy: TransactionStrategy,
        persistor: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        let result = self
            .execute_single_stage(&transaction_strategy, persistor)
            .await;
        match result {
            Ok(value) => Ok(value),
            Err(err) => {
                todo!()
            }
        }
    }

    /// Attempts and retries to execute strategy and parses errors
    async fn execute_strategy_with_retries<P: IntentPersister>(
        &self,
        transaction_strategy: &TransactionStrategy,
        persister: &Option<P>,
    ) -> IntentExecutorResult<Signature, TransactionStrategyExecutionError>
    {
        const RETRY_FOR: Duration = Duration::from_secs(2 * 60);
        const SLEEP: Duration = Duration::from_millis(500);

        let convert_transaction_error =
            |err: TransactionError,
             map: fn(
                solana_rpc_client_api::client_error::Error,
            ) -> MagicBlockRpcClientError|
             -> TransactionStrategyExecutionError {
                // There's always 2 budget instructions in front
                const OFFSET: u8 = 2;
                const OUTDATED_SLOT: u32 =
                    dlp::error::DlpError::OutdatedSlot as u32;

                match err {
                    TransactionError::InstructionError(
                        _,
                        InstructionError::Custom(OUTDATED_SLOT),
                    ) => TransactionStrategyExecutionError::CommitIDError,
                    TransactionError::InstructionError(
                        _,
                        InstructionError::MaxInstructionTraceLengthExceeded,
                    ) => TransactionStrategyExecutionError::CpiLimitError,
                    TransactionError::InstructionError(index, ix_err) => {
                        let transaction_error =
                            TransactionError::InstructionError(index, ix_err);
                        let internal_err =
                            TransactionStrategyExecutionError::InternalError(
                                InternalError::MagicBlockRpcClientError(map(
                                    transaction_error.into(),
                                )),
                            );

                        let Some(action_index) = index.checked_sub(OFFSET)
                        else {
                            return internal_err;
                        };

                        if let Some(TaskType::Action) = transaction_strategy
                            .optimized_tasks
                            .get(action_index as usize)
                            .map(|task: &Box<dyn BaseTask>| task.task_type())
                        {
                            TransactionStrategyExecutionError::ActionsError
                        } else {
                            internal_err
                        }
                    }
                    err => TransactionStrategyExecutionError::InternalError(
                        InternalError::MagicBlockRpcClientError(
                            map(err.into()),
                        ),
                    ),
                }
            };

        let conver_rpc_error = |err: solana_rpc_client_api::client_error::Error| -> TransactionStrategyExecutionError {
            todo!()
        };

        // TransactionPreparator retries internally,
        // so we can skip retries here
        // More importantly errors from it can't be handled so we propagate them
        // TODO: move outside
        let prepared_message = self
            .transaction_preparator
            .prepare_for_strategy(
                &self.authority,
                transaction_strategy,
                persister,
            )
            .await
            .map_err(IntentExecutorError::FailedFinalizePreparationError)?;

        let mut last_err;
        let start = Instant::now();
        while start.elapsed() < RETRY_FOR {
            let result =
                self.send_prepared_message(prepared_message.clone()).await;
            // .map_err(|(err, signature)| {
            //     IntentExecutorError::FailedToCommitError { err, signature }
            // })?;

            match result {
                Ok(result) => {
                    return match result.into_result() {
                        Ok(value) =>  Ok(value),
                        Err(err) => {
                            // Since err is TransactionError we return from here right away
                            // It's wether some known reason like: ActionError/CommitIdError or something else
                            // We can't recover here so we propagate
                            Err(convert_transaction_error(err, MagicBlockRpcClientError::SendTransaction))
                        }
                    }
                }
                Err(err @ InternalError::SignerError(_)) => {
                    // Can't handle SignerError in any way
                    // propagate lower
                    return Err(TransactionStrategyExecutionError::InternalError(err))
                }
                Err(InternalError::MagicBlockRpcClientError(MagicBlockRpcClientError::RpcClientError(err))) => {
                    match err.kind {
                        err_kind @ ErrorKind::Io(_) => {
                            last_err =  TransactionStrategyExecutionError::InternalError(InternalError::MagicBlockRpcClientError(MagicBlockRpcClientError::RpcClientError(
                                solana_rpc_client_api::client_error::Error {
                                    request: err.request,
                                    kind: err_kind
                                }
                            )))
                        }
                        err_kind @ ErrorKind::Reqwest(_) => {
                            // Can't handle - propagate
                            return Err(TransactionStrategyExecutionError::InternalError(InternalError::MagicBlockRpcClientError(MagicBlockRpcClientError::RpcClientError(
                                solana_rpc_client_api::client_error::Error {
                                    request: err.request,
                                    kind: err_kind
                                }
                            ))))
                        }
                        err_kind @ ErrorKind::Middleware(_) => {
                            // Can't handle - propagate
                            return Err(TransactionStrategyExecutionError::InternalError(InternalError::MagicBlockRpcClientError(MagicBlockRpcClientError::RpcClientError(
                                solana_rpc_client_api::client_error::Error {
                                    request: err.request,
                                    kind: err_kind
                                }
                            ))))
                        }
                        err_kind @ ErrorKind::RpcError(_) => {
                            // Can't handle - propagate
                            return Err(TransactionStrategyExecutionError::InternalError(InternalError::MagicBlockRpcClientError(MagicBlockRpcClientError::RpcClientError(
                                solana_rpc_client_api::client_error::Error {
                                    request: err.request,
                                    kind: err_kind
                                }
                            ))))
                        }
                        err_kind @ ErrorKind::SerdeJson(_) => {
                            // Can't handle - propagate
                            return Err(TransactionStrategyExecutionError::InternalError(InternalError::MagicBlockRpcClientError(MagicBlockRpcClientError::RpcClientError(
                                solana_rpc_client_api::client_error::Error {
                                    request: err.request,
                                    kind: err_kind
                                }
                            ))))
                        }
                        err_kind @ ErrorKind::SigningError(_) => {
                            // Can't handle - propagate
                            return Err(TransactionStrategyExecutionError::InternalError(InternalError::MagicBlockRpcClientError(MagicBlockRpcClientError::RpcClientError(
                                solana_rpc_client_api::client_error::Error {
                                    request: err.request,
                                    kind: err_kind
                                }
                            ))))
                        }
                        ErrorKind::TransactionError(err) => return Err(convert_transaction_error(err, MagicBlockRpcClientError::RpcClientError)),
                        err_kind @ ErrorKind::Custom(_) => {
                            // Can't handle - propagate
                            return Err(TransactionStrategyExecutionError::InternalError(InternalError::MagicBlockRpcClientError(MagicBlockRpcClientError::RpcClientError(
                                solana_rpc_client_api::client_error::Error {
                                    request: err.request,
                                    kind: err_kind
                                }
                            ))))
                        }
                    }
                }
                Err(InternalError::MagicBlockRpcClientError(MagicBlockRpcClientError::SendTransaction(err)))
                => {
                    match err.kind {
                        err_kind @ ErrorKind::Io(_) => {
                            last_err =  TransactionStrategyExecutionError::InternalError(InternalError::MagicBlockRpcClientError(MagicBlockRpcClientError::RpcClientError(
                                solana_rpc_client_api::client_error::Error {
                                    request: err.request,
                                    kind: err_kind
                                }
                            )))
                        }
                        err_kind @ ErrorKind::Reqwest(_) => {
                            // Can't handle - propagate
                            return Err(TransactionStrategyExecutionError::InternalError(InternalError::MagicBlockRpcClientError(MagicBlockRpcClientError::RpcClientError(
                                solana_rpc_client_api::client_error::Error {
                                    request: err.request,
                                    kind: err_kind
                                }
                            ))))
                        }
                        err_kind @ ErrorKind::Middleware(_) => {
                            // Can't handle - propagate
                            return Err(TransactionStrategyExecutionError::InternalError(InternalError::MagicBlockRpcClientError(MagicBlockRpcClientError::RpcClientError(
                                solana_rpc_client_api::client_error::Error {
                                    request: err.request,
                                    kind: err_kind
                                }
                            ))))
                        }
                        err_kind @ ErrorKind::RpcError(_) => {
                            // Can't handle - propagate
                            return Err(TransactionStrategyExecutionError::InternalError(InternalError::MagicBlockRpcClientError(MagicBlockRpcClientError::RpcClientError(
                                solana_rpc_client_api::client_error::Error {
                                    request: err.request,
                                    kind: err_kind
                                }
                            ))))
                        }
                        err_kind @ ErrorKind::SerdeJson(_) => {
                            // Can't handle - propagate
                            return Err(TransactionStrategyExecutionError::InternalError(InternalError::MagicBlockRpcClientError(MagicBlockRpcClientError::RpcClientError(
                                solana_rpc_client_api::client_error::Error {
                                    request: err.request,
                                    kind: err_kind
                                }
                            ))))
                        }
                        err_kind @ ErrorKind::SigningError(_) => {
                            // Can't handle - propagate
                            return Err(TransactionStrategyExecutionError::InternalError(InternalError::MagicBlockRpcClientError(MagicBlockRpcClientError::RpcClientError(
                                solana_rpc_client_api::client_error::Error {
                                    request: err.request,
                                    kind: err_kind
                                }
                            ))))
                        }
                        ErrorKind::TransactionError(err) => return Err(convert_transaction_error(err, MagicBlockRpcClientError::RpcClientError)),
                        err_kind @ ErrorKind::Custom(_) => {
                            // Can't handle - propagate
                            return Err(TransactionStrategyExecutionError::InternalError(InternalError::MagicBlockRpcClientError(MagicBlockRpcClientError::RpcClientError(
                                solana_rpc_client_api::client_error::Error {
                                    request: err.request,
                                    kind: err_kind
                                }
                            ))))
                        }
                    }
                }
                Err(InternalError::MagicBlockRpcClientError(err @ MagicBlockRpcClientError::GetLatestBlockhash(_))) => {
                    // we're retrying in that case
                    last_err = err;
                }
                Err(InternalError::MagicBlockRpcClientError(err @ MagicBlockRpcClientError::GetSlot(_))) => {
                    // Unexpected error, returning right away
                    warn!("MagicBlockRpcClientError::GetSlot during send transaction");
                    return Err(TransactionStrategyExecutionError::InternalError(err.into()))
                }
                Err(InternalError::MagicBlockRpcClientError(err @ MagicBlockRpcClientError::LookupTableDeserialize(_))) => {
                    // Unexpected error, returning right away
                    warn!(" MagicBlockRpcClientError::LookupTableDeserialize during send transaction");
                    return Err(TransactionStrategyExecutionError::InternalError(err.into()))
                }
                err @ Err(InternalError::MagicBlockRpcClientError(MagicBlockRpcClientError::CannotGetTransactionSignatureStatus(_, _))) => {
                    // if there's still time left we can retry sending tx
                    last_err = err;
                    continue
                }
                err @ Err(InternalError::MagicBlockRpcClientError(MagicBlockRpcClientError::CannotConfirmTransactionSignatureStatus(_, _))) => {
                    // if there's still time left we can retry sending tx
                    // Since [`DEFAULT_MAX_TIME_TO_PROCESSED`] is large we skip sleep as well
                    last_err = err;
                    continue
                }
                err @ Err(InternalError::MagicBlockRpcClientError(MagicBlockRpcClientError::SentTransactionError(_, _))) => {
                    // if there's still time left we can retry sending tx
                    // Since [`DEFAULT_MAX_TIME_TO_PROCESSED`] is large we skip sleep as well
                    last_err = err;
                    continue
                }
            };

            sleep(SLEEP).await
        }

        Err(last_err)
    }

    /// Optimization: executes Intent in single stage
    /// where Commit & Finalize are united
    // TODO: remove once challenge window introduced
    async fn execute_single_stage<P: IntentPersister>(
        &self,
        transaction_strategy: &TransactionStrategy,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        let prepared_message = self
            .transaction_preparator
            .prepare_for_strategy(
                &self.authority,
                transaction_strategy,
                persister,
            )
            .await
            .map_err(IntentExecutorError::FailedFinalizePreparationError)?;

        let signature = self
            .send_prepared_message(prepared_message)
            .await
            .map_err(|(err)| {
                let signature = err.signature();
                IntentExecutorError::FailedToCommitError { err, signature }
            })?
            .into_signature();

        debug!("Single stage intent executed: {}", signature);
        Ok(ExecutionOutput::SingleStage(signature))
    }

    /// Executes Intent in 2 stage: Commit & Finalize
    async fn execute_two_stages<P: IntentPersister>(
        &self,
        commit_strategy: &TransactionStrategy,
        finalize_strategy: &TransactionStrategy,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        // Prepare everything for Commit stage execution
        let result = self
            .transaction_preparator
            .prepare_for_strategy(&self.authority, commit_strategy, persister)
            .await;

        // TODO: improve this, the thing is from delivery preparator we shall get only RpcError
        let prepared_commit_message = match result {
            Ok(value) => Ok(value),
            Err(TransactionPreparatorError::FailedToFitError) => {
                unreachable!("This is checked in TaskBuilder!")
            }
            err @ Err(TransactionPreparatorError::SignerError(_)) => err,
            err @ Err(TransactionPreparatorError::DeliveryPreparationError(
                _,
            )) => err,
        }
        .map_err(IntentExecutorError::FailedCommitPreparationError)?;

        let commit_signature = self
            .send_prepared_message(prepared_commit_message)
            .await
            .map_err(|err| {
                let signature = err.signature();
                IntentExecutorError::FailedToCommitError { err, signature }
            })?
            .into_signature();
        debug!("Commit stage succeeded: {}", commit_signature);

        // Prepare everything for Finalize stage execution
        let prepared_finalize_message = self
            .transaction_preparator
            .prepare_for_strategy(&self.authority, finalize_strategy, persister)
            .await
            .map_err(IntentExecutorError::FailedFinalizePreparationError)?;

        let finalize_signature = self
            .send_prepared_message(prepared_finalize_message)
            .await
            .map_err(|err| {
                let finalize_signature = err.signature();
                IntentExecutorError::FailedToFinalizeError {
                    err,
                    commit_signature: Some(commit_signature),
                    finalize_signature,
                }
            })?
            .into_signature();
        debug!("Finalize stage succeeded: {}", finalize_signature);

        Ok(ExecutionOutput::TwoStage {
            commit_signature,
            finalize_signature,
        })
    }

    /// Shared helper for sending transactions
    async fn send_prepared_message(
        &self,
        mut prepared_message: VersionedMessage,
    ) -> IntentExecutorResult<MagicBlockSendTransactionOutcome, InternalError>
    {
        let latest_blockhash = self.rpc_client.get_latest_blockhash().await?;
        match &mut prepared_message {
            VersionedMessage::V0(value) => {
                value.recent_blockhash = latest_blockhash;
            }
            VersionedMessage::Legacy(value) => {
                warn!("TransactionPreparator v1 does not use Legacy message");
                value.recent_blockhash = latest_blockhash;
            }
        };

        let transaction = VersionedTransaction::try_new(
            prepared_message,
            &[&self.authority],
        )?;
        let result = self
            .rpc_client
            .send_transaction(
                &transaction,
                &MagicBlockSendTransactionConfig::ensure_committed(),
            )
            .await?;

        Ok(result)
    }

    fn persist_result<P: IntentPersister>(
        persistor: &P,
        result: &IntentExecutorResult<ExecutionOutput>,
        message_id: u64,
        pubkeys: &[Pubkey],
    ) {
        let update_status = match result {
            Ok(value) => {
                let signatures = match *value {
                    ExecutionOutput::SingleStage(signature) => {
                        CommitStatusSignatures {
                            commit_stage_signature: signature,
                            finalize_stage_signature: Some(signature),
                        }
                    }
                    ExecutionOutput::TwoStage {
                        commit_signature,
                        finalize_signature,
                    } => CommitStatusSignatures {
                        commit_stage_signature: commit_signature,
                        finalize_stage_signature: Some(finalize_signature),
                    },
                };
                let update_status = CommitStatus::Succeeded(signatures);
                persist_status_update_by_message_set(
                    persistor,
                    message_id,
                    pubkeys,
                    update_status,
                );

                if let Err(err) =
                    persistor.finalize_base_intent(message_id, *value)
                {
                    error!("Failed to persist ExecutionOutput: {}", err);
                }

                return;
            }
            Err(IntentExecutorError::EmptyIntentError)
            | Err(IntentExecutorError::FailedToFitError)
            | Err(IntentExecutorError::TaskBuilderError(_))
            | Err(IntentExecutorError::FailedCommitPreparationError(
                TransactionPreparatorError::SignerError(_),
            ))
            | Err(IntentExecutorError::FailedFinalizePreparationError(
                TransactionPreparatorError::SignerError(_),
            )) => Some(CommitStatus::Failed),
            Err(IntentExecutorError::FailedCommitPreparationError(
                TransactionPreparatorError::FailedToFitError,
            )) => Some(CommitStatus::PartOfTooLargeBundleToProcess),
            Err(IntentExecutorError::FailedCommitPreparationError(
                TransactionPreparatorError::DeliveryPreparationError(_),
            )) => {
                // Intermediate commit preparation progress recorded by DeliveryPreparator
                None
            }
            Err(IntentExecutorError::FailedToCommitError {
                err: _,
                signature,
            }) => {
                // Commit is a single TX, so if it fails, all of commited accounts marked FailedProcess
                let status_signature =
                    signature.map(|sig| CommitStatusSignatures {
                        commit_stage_signature: sig,
                        finalize_stage_signature: None,
                    });
                Some(CommitStatus::FailedProcess(status_signature))
            }
            Err(IntentExecutorError::FailedFinalizePreparationError(_)) => {
                // Not supported in persistor
                None
            }
            Err(IntentExecutorError::FailedToFinalizeError {
                err: _,
                commit_signature,
                finalize_signature,
            }) => {
                // Finalize is a single TX, so if it fails, all of commited accounts marked FailedFinalize
                let update_status =
                    if let Some(commit_signature) = commit_signature {
                        let signatures = CommitStatusSignatures {
                            commit_stage_signature: *commit_signature,
                            finalize_stage_signature: *finalize_signature,
                        };
                        CommitStatus::FailedFinalize(signatures)
                    } else {
                        CommitStatus::FailedProcess(None)
                    };

                Some(update_status)
            }
            Err(IntentExecutorError::SignerError(_)) => {
                Some(CommitStatus::Failed)
            }
        };

        if let Some(update_status) = update_status {
            persist_status_update_by_message_set(
                persistor,
                message_id,
                pubkeys,
                update_status,
            );
        }
    }

    // fn handle_result(
    //     result: &IntentExecutorResult<ExecutionOutput>,
    //     message_id: u64,
    //     pubkeys: &[Pubkey],
    // ) {
    //     match result {
    //         Ok(val) => {}
    //     }
    // }
}

#[async_trait]
impl<T, C> IntentExecutor for IntentExecutorImpl<T, C>
where
    T: TransactionPreparator,
    C: TaskInfoFetcher,
{
    /// Executes Message on Base layer
    /// Returns `ExecutionOutput` or an `Error`
    async fn execute<P: IntentPersister>(
        &self,
        base_intent: ScheduledBaseIntent,
        persister: Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput> {
        let message_id = base_intent.id;
        let pubkeys = base_intent.get_committed_pubkeys();

        let result = self.execute_inner(&base_intent, &persister).await;
        if let Some(pubkeys) = pubkeys {
            Self::persist_result(&persister, &result, message_id, &pubkeys);
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use solana_pubkey::Pubkey;

    use crate::{
        intent_execution_manager::intent_scheduler::create_test_intent,
        intent_executor::{
            task_info_fetcher::{
                ResetType, TaskInfoFetcher, TaskInfoFetcherResult,
            },
            IntentExecutorImpl,
        },
        persist::IntentPersisterImpl,
        tasks::task_builder::{TaskBuilderV1, TasksBuilder},
        transaction_preparator::transaction_preparator::TransactionPreparatorV1,
    };

    struct MockInfoFetcher;
    #[async_trait::async_trait]
    impl TaskInfoFetcher for MockInfoFetcher {
        async fn fetch_next_commit_ids(
            &self,
            pubkeys: &[Pubkey],
        ) -> TaskInfoFetcherResult<HashMap<Pubkey, u64>> {
            Ok(pubkeys.iter().map(|pubkey| (*pubkey, 0)).collect())
        }

        async fn fetch_rent_reimbursements(
            &self,
            pubkeys: &[Pubkey],
        ) -> TaskInfoFetcherResult<Vec<Pubkey>> {
            Ok(pubkeys.iter().map(|_| Pubkey::new_unique()).collect())
        }

        fn peek_commit_id(&self, _pubkey: &Pubkey) -> Option<u64> {
            Some(0)
        }

        fn reset(&self, reset_type: ResetType) {}
    }

    // Flow
    // We attempt an Intent execution
    // 1. We create tasks
    //     Err. Retry for 1 minute. Still fails - terrible error, return. Can't recover
    //          The errors here: Metadata Record not found/invalid or RPC is broken
    // 2. We build a strategy
    //  Err. Critical error which shouldn't happen because of smart contract check(doesn't exist)
    //      No retry here, exit
    //
    // 3. Prepare for delivery
    //  Err. We can get only RPC related issues here or TableMania(also RPC).
    //      We can only retry but if still no luck - fail. Can't be recovered
    // 4. Send transaction
    //  Err. Here we could have: ActionError, CommitIdError, CpiLimitError, RpcError
    //      ActionError: based on config strip away actions & just commit/return ActionError
    //          User will have an ability to specify if actions are mandatory so if set we return ActionError
    //      CommitIdError: some other process could mess with our CommitId and commit concurrently, hence invalidating our CommitId
    //          Reset TaskInfoFetcher - retry
    //      CpiLimitError:
    //          a. SingleStage - switch to TwoStage & retry. Reuse already created buffers and ALTs
    //          b. TwoStage - if mandatory commit set - strip away Actions - retry,
    //              otherwise fail(actually we shouldn't allow this on first place, should be checked on contract)
    //                i. If still fail after retry: return CpiLimitError(shouldn't allow this on first place, should be checked on contract)
    //      RpcError/AnyOtherError:
    //          Nothing we can do other than retry :(

    #[tokio::test]
    async fn test_try_unite() {
        let pubkey = [Pubkey::new_unique()];
        let intent = create_test_intent(0, &pubkey);

        let info_fetcher = Arc::new(MockInfoFetcher);
        let commit_task = TaskBuilderV1::commit_tasks(
            &info_fetcher,
            &intent,
            &None::<IntentPersisterImpl>,
        )
        .await
        .unwrap();
        let finalize_task =
            TaskBuilderV1::finalize_tasks(&info_fetcher, &intent)
                .await
                .unwrap();

        let result = IntentExecutorImpl::<
            TransactionPreparatorV1,
            MockInfoFetcher,
        >::try_unite_tasks(
            &commit_task,
            &finalize_task,
            &Pubkey::new_unique(),
            &None::<IntentPersisterImpl>,
        );

        let strategy = result.unwrap().unwrap();
        assert!(strategy.lookup_tables_keys.is_empty());
    }
}
