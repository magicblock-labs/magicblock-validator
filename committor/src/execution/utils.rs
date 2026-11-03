use crate::{
    execution::{
        ExecutionOutput, IntentExecutionReport,
        error::{IntentExecutorError, IntentExecutorResult},
    },
    shared::outbox::{OutboxClient, ScheduledBaseIntentMeta},
};

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
    if let Err(err) = &result
        && !is_terminal_execution_error(err)
    {
        return result;
    }

    let transaction = std::mem::take(&mut meta.intent_sent_transaction);
    let payload = crate::execution::report::build_sent_commit(meta, &result, execution_report);
    outbox_client.notify_commit_sent(payload, transaction).await?;

    result
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
