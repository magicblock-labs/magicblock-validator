use std::sync::Arc;

use futures_util::future::join;
use program::magic_scheduled_base_intent::ScheduledIntentBundle;

use crate::{
    execution::{
        ExecutionOutput, IntentExecutionReport,
        error::{IntentExecutorError, IntentExecutorResult},
        task_builder::TaskBuilder,
    },
    shared::{
        account_state::TaskInfoProvider,
        outbox::{OutboxClient, ScheduledBaseIntentMeta},
        tasks::Task,
    },
};

pub(in crate::execution) async fn build_intent_tasks<F: TaskInfoProvider>(
    intent_bundle: &ScheduledIntentBundle,
    task_info_provider: &Arc<F>,
) -> IntentExecutorResult<(Vec<Task>, Vec<Task>)> {
    let commit_tasks_fut = TaskBuilder::commit_tasks(task_info_provider, intent_bundle);
    let follow_up_tasks_fut = TaskBuilder::follow_up_tasks(task_info_provider, intent_bundle);
    let (commit_tasks, follow_up_tasks) = join(commit_tasks_fut, follow_up_tasks_fut).await;

    Ok((commit_tasks?, follow_up_tasks?))
}

/// Reports terminal intent execution outcomes. The `ScheduledCommitSent`
/// instruction closes the outbox record atomically with the report, so
/// retryable lifecycle failures intentionally leave the outbox account open.
/// Note: we consider failed reporting to Outbox as overall intent failure
/// as it is part of intent execution lifecycle.
pub(in crate::execution) async fn report_and_close_intent(
    result: IntentExecutorResult<ExecutionOutput>,
    mut meta: ScheduledBaseIntentMeta,
    execution_report: &mut IntentExecutionReport,
    outbox_client: &dyn OutboxClient,
) -> IntentExecutorResult<ExecutionOutput> {
    if !should_report_to_outbox(&result) {
        return result;
    }

    let transaction = std::mem::take(&mut meta.intent_sent_transaction);
    let payload = crate::execution::report::build_sent_commit(meta, &result, execution_report);
    outbox_client.notify_commit_sent(payload, transaction).await?;

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
        IntentExecutorError::FailedExecution(err) => !err.is_transient(),
        IntentExecutorError::TaskBuilderError(err) => !err.is_transient(),
        IntentExecutorError::EmptyIntentError
        | IntentExecutorError::FailedToFitError
        | IntentExecutorError::CompileError(_)
        | IntentExecutorError::SignerError(_) => true,
    }
}
