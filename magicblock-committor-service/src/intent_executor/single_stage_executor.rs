use std::{ops::ControlFlow, sync::Arc};

use magicblock_core::traits::{ActionError, ActionsCallbackScheduler};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;
use tracing::{error, instrument};

use crate::{
    intent_executor::{
        error::{
            IntentExecutorError, IntentExecutorResult,
            TransactionStrategyExecutionError,
        },
        intent_execution_client::IntentExecutionClient,
        task_info_fetcher::{CacheTaskInfoFetcher, TaskInfoFetcher},
        utils::{
            handle_actions_result, handle_commit_id_error,
            handle_undelegation_error, prepare_and_execute_strategy,
        },
        ExecutionOutput, IntentExecutionReport,
    },
    persist::{IntentPersister, IntentPersisterImpl},
    tasks::{
        commit_task::CommitTask, task_strategist::TransactionStrategy,
        BaseTaskImpl, FinalizeTask,
    },
    transaction_preparator::TransactionPreparator,
};

pub struct SingleStageExecutor<'a, F, A> {
    current_attempt: u8,
    execution_report: &'a mut IntentExecutionReport,

    authority: Keypair,
    intent_client: IntentExecutionClient,
    task_info_fetcher: Arc<CacheTaskInfoFetcher<F>>,
    callback_scheduler: A,
    transaction_strategy: TransactionStrategy,
}

impl<'a, F, A> SingleStageExecutor<'a, F, A>
where
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
{
    pub fn new(
        authority: Keypair,
        intent_client: IntentExecutionClient,
        task_info_fetcher: Arc<CacheTaskInfoFetcher<F>>,
        transaction_strategy: TransactionStrategy,
        callback_scheduler: A,
        execution_report: &'a mut IntentExecutionReport,
    ) -> Self {
        Self {
            current_attempt: 0,
            authority,
            intent_client,
            task_info_fetcher,
            transaction_strategy,
            callback_scheduler,
            execution_report,
        }
    }

    #[instrument(
        skip(self, committed_pubkeys, transaction_preparator, persister),
        fields(stage = "single_stage")
    )]
    pub async fn execute<T, P>(
        &mut self,
        committed_pubkeys: &[Pubkey],
        transaction_preparator: &T,
        persister: &Option<P>,
    ) -> IntentExecutorResult<ExecutionOutput>
    where
        T: TransactionPreparator,
        P: IntentPersister,
    {
        const RECURSION_CEILING: u8 = 10;

        let result = loop {
            self.current_attempt += 1;

            // Prepare & execute message
            let execution_result = prepare_and_execute_strategy(
                &self.intent_client,
                &self.authority,
                transaction_preparator,
                &mut self.transaction_strategy,
                persister,
            )
            .await
            .map_err(IntentExecutorError::FailedFinalizePreparationError)?;

            // Process error: Ok - return, Err - handle further
            let execution_err = match execution_result {
                // break with result, strategy that was executed at this point has to be returned for cleanup
                Ok(value) => {
                    break Ok(ExecutionOutput::SingleStage(value));
                }
                Err(err) => err,
            };

            // Attempt patching
            let flow = self
                .patch_strategy(
                    &execution_err,
                    committed_pubkeys,
                    transaction_preparator,
                )
                .await?;
            let cleanup = match flow {
                ControlFlow::Continue(cleanup) => cleanup,
                ControlFlow::Break(()) => {
                    break Err(execution_err);
                }
            };
            self.execution_report.dispose(cleanup);

            if self.current_attempt >= RECURSION_CEILING {
                error!(
                    attempt = self.current_attempt,
                    ceiling = RECURSION_CEILING,
                    error = ?execution_err,
                    "Recursion ceiling exceeded"
                );
                break Err(execution_err);
            } else {
                self.execution_report.add_patched_error(execution_err);
            }
        };

        result.map_err(|err| {
            IntentExecutorError::from_finalize_execution_error(
                err,
                // TODO(edwin): shall one stage have same signature for commit & finalize
                None,
            )
        })
    }

    pub fn has_callbacks(&self) -> bool {
        self.transaction_strategy.has_actions_callbacks()
    }

    pub fn execute_callbacks(
        &mut self,
        result: Result<(), impl Into<ActionError>>,
    ) {
        let junk_strategy = handle_actions_result(
            &self.authority.pubkey(),
            &self.callback_scheduler,
            self.execution_report,
            &mut self.transaction_strategy,
            result.map_err(|err| err.into()),
        );
        self.execution_report.dispose(junk_strategy);
    }

    pub fn consume_strategy(self) -> TransactionStrategy {
        self.transaction_strategy
    }

    /// Patch the current `transaction_strategy` in response to a recoverable
    /// [`TransactionStrategyExecutionError`], optionally preparing cleanup data
    /// to be applied after a retry.
    ///
    /// [`TransactionStrategyExecutionError`], returning either:
    /// - `Continue(to_cleanup)` when a retry should be attempted with cleanup metadata, or
    /// - `Break(())` when this stage cannot be recovered here.
    pub async fn patch_strategy<T: TransactionPreparator>(
        &mut self,
        err: &TransactionStrategyExecutionError,
        committed_pubkeys: &[Pubkey],
        transaction_preparator: &T,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        if committed_pubkeys.is_empty() {
            // No patching is applicable if intent doesn't commit accounts
            return Ok(ControlFlow::Break(()));
        }

        match err {
            TransactionStrategyExecutionError::ActionsError(err, signature) => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let action_error = Err(ActionError::ActionsError(err.clone(), *signature));
                let to_cleanup = handle_actions_result(
                    &self.authority.pubkey(),
                    &self.callback_scheduler,
                    self.execution_report,
                    &mut self.transaction_strategy,
                    action_error,
                );
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::CommitIDError(_, _) => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let to_cleanup = handle_commit_id_error(
                        &self.authority.pubkey(),
                        &self.task_info_fetcher,
                        committed_pubkeys,
                        &mut self.transaction_strategy,
                    )
                    .await?;
                Ok(ControlFlow::Continue(to_cleanup))
            }
            err
            @ TransactionStrategyExecutionError::UnfinalizedAccountError(
                _,
                signature,
            ) => {
                let optimized_tasks =
                    self.transaction_strategy.optimized_tasks.as_slice();
                if let Some(BaseTaskImpl::Commit(task)) = err
                    .task_index()
                    .and_then(|index| optimized_tasks.get(index as usize))
                {
                    self.handle_unfinalized_account_error(
                        signature, task, transaction_preparator
                    )
                    .await
                } else {
                    error!(
                        task_index = err.task_index(),
                        optimized_tasks_len = optimized_tasks.len(),
                        error = ?err,
                        "RPC returned unexpected task index"
                    );
                    Ok(ControlFlow::Break(()))
                }
            }
            TransactionStrategyExecutionError::UndelegationError(_, _) => {
                // Here we patch strategy for it to be retried in next iteration
                // & we also record data that has to be cleaned up after patch
                let to_cleanup = handle_undelegation_error(
                    &self.authority.pubkey(),
                    &mut self.transaction_strategy
                );
                Ok(ControlFlow::Continue(to_cleanup))
            }
            TransactionStrategyExecutionError::CpiLimitError(_, _)
            | TransactionStrategyExecutionError::LoadedAccountsDataSizeExceeded(_, _) => {
                // Can't be handled in scope of single stage execution
                // We signal flow break
                Ok(ControlFlow::Break(()))
            }
            TransactionStrategyExecutionError::InternalError(_) => {
                // Error that we can't handle - break with cleanup data
                Ok(ControlFlow::Break(()))
            }
        }
    }

    /// Handles unfinalized account error
    /// Sends a separate tx to finalize account and then continues execution
    async fn handle_unfinalized_account_error<T: TransactionPreparator>(
        &self,
        failed_signature: &Option<Signature>,
        task: &CommitTask,
        transaction_preparator: &T,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>> {
        let finalize_task: BaseTaskImpl = FinalizeTask {
            delegated_account: task.committed_account.pubkey,
        }
        .into();
        prepare_and_execute_strategy(
            &self.intent_client,
            &self.authority,
            transaction_preparator,
            &mut TransactionStrategy {
                optimized_tasks: vec![finalize_task],
                lookup_tables_keys: vec![],
            },
            &None::<IntentPersisterImpl>,
        )
        .await
        .map_err(IntentExecutorError::FailedFinalizePreparationError)?
        .map_err(|err| IntentExecutorError::FailedToFinalizeError {
            err,
            commit_signature: None,
            finalize_signature: *failed_signature,
        })?;

        Ok(ControlFlow::Continue(TransactionStrategy {
            optimized_tasks: vec![],
            lookup_tables_keys: vec![],
        }))
    }
}
