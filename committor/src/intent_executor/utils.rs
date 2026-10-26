use std::sync::Arc;

use futures_util::future::join;
use program::magic_scheduled_base_intent::ScheduledIntentBundle;

use crate::{
    intent_executor::{
        ExecutionOutput, IntentExecutionReport,
        error::{IntentExecutorError, IntentExecutorResult},
        strategy_executor::error::TransactionStrategyExecutionError,
    },
    outbox::{OutboxClient, ScheduledBaseIntentMeta},
    tasks::{
        BaseTaskImpl,
        task_builder::{TaskBuilderImpl, TasksBuilder},
        task_info_fetcher::TaskInfoFetcher,
    },
};

pub(in crate::intent_executor) async fn build_intent_tasks<F: TaskInfoFetcher>(
    intent_bundle: &ScheduledIntentBundle,
    task_info_fetcher: &Arc<F>,
) -> IntentExecutorResult<(Vec<BaseTaskImpl>, Vec<BaseTaskImpl>)> {
    let commit_tasks_fut = TaskBuilderImpl::commit_tasks(task_info_fetcher, intent_bundle);
    let follow_up_tasks_fut = TaskBuilderImpl::follow_up_tasks(task_info_fetcher, intent_bundle);
    let (commit_tasks, follow_up_tasks) = join(commit_tasks_fut, follow_up_tasks_fut).await;

    Ok((commit_tasks?, follow_up_tasks?))
}

/// Reports terminal intent execution outcomes. The `ScheduledCommitSent`
/// instruction closes the outbox record atomically with the report, so
/// retryable lifecycle failures intentionally leave the outbox account open.
/// Note: we consider failed reporting to Outbox as overall intent failure
/// as it is part of intent execution lifecycle.
pub(in crate::intent_executor) async fn report_and_close_intent<O>(
    result: IntentExecutorResult<ExecutionOutput>,
    meta: ScheduledBaseIntentMeta,
    execution_report: &mut IntentExecutionReport,
    outbox_client: &O,
) -> IntentExecutorResult<ExecutionOutput>
where
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    if !should_report_to_outbox(&result) {
        return result;
    }

    outbox_client
        .notify_commit_sent(meta, &result, execution_report)
        .await
        .map_err(Into::into)?;

    result
}

fn should_report_to_outbox(result: &IntentExecutorResult<ExecutionOutput>) -> bool {
    match result {
        Ok(_) => true,
        Err(err) => is_terminal_execution_error(err),
    }
}

fn is_terminal_execution_error(err: &IntentExecutorError) -> bool {
    match err {
        IntentExecutorError::OutboxClientError(_)
        | IntentExecutorError::GetPendingSignatureStatusError(_)
        | IntentExecutorError::PendingSignatureResolutionError(_)
        | IntentExecutorError::FailedPreparation(_)
        | IntentExecutorError::SchedulerError(_)
        | IntentExecutorError::PoisonedIntentError => false,
        IntentExecutorError::FailedExecution(err) => is_terminal_strategy_error(err),
        IntentExecutorError::TaskBuilderError(err) => !err.is_transient(),
        IntentExecutorError::EmptyIntentError
        | IntentExecutorError::FailedToFitError
        | IntentExecutorError::CompileError(_)
        | IntentExecutorError::SignerError(_) => true,
    }
}

fn is_terminal_strategy_error(err: &TransactionStrategyExecutionError) -> bool {
    !err.is_transient()
}
