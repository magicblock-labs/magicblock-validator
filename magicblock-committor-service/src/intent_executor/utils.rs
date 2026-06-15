use std::{sync::Arc, time::Duration};

use futures_util::future::join;
use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::magic_scheduled_base_intent::ScheduledIntentBundle;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;

use crate::{
    intent_executor::{
        error::{IntentExecutorError, IntentExecutorResult},
        strategy_executor::{
            two_stage,
            two_stage::TwoStageStrategyExecutor,
            utils::{execute_with_timeout, CommitStage, FinalizeStage},
        },
        task_info_fetcher::TaskInfoFetcher,
        ExecutionOutput, IntentExecutionReport, IntentExecutorCtx,
    },
    outbox_client::OutboxClient,
    tasks::{
        task_builder::{TaskBuilderImpl, TasksBuilder},
        task_strategist::TransactionStrategy,
        BaseTaskImpl,
    },
    transaction_preparator::TransactionPreparator,
};

pub(in crate::intent_executor) async fn build_commit_finalize_tasks<
    F: TaskInfoFetcher,
>(
    intent_bundle: &ScheduledIntentBundle,
    task_info_fetcher: &Arc<F>,
) -> IntentExecutorResult<(Vec<BaseTaskImpl>, Vec<BaseTaskImpl>)> {
    let commit_tasks_fut =
        TaskBuilderImpl::commit_tasks(&task_info_fetcher, &intent_bundle);
    let finalize_tasks_fut =
        TaskBuilderImpl::finalize_tasks(&task_info_fetcher, &intent_bundle);
    let (commit_tasks, finalize_tasks) =
        join(commit_tasks_fut, finalize_tasks_fut).await;

    Ok((commit_tasks?, finalize_tasks?))
}

pub(in crate::intent_executor) async fn execute_two_stage_flow<T, F, A, O>(
    ctx: &IntentExecutorCtx<T, F, A, O>,
    state: two_stage::Initialized,
    authority: &Keypair,
    intent_id: u64,
    committed_pubkeys: &[Pubkey],
    execution_report: &mut IntentExecutionReport,
    time_left: impl Fn() -> Option<Duration>,
) -> IntentExecutorResult<ExecutionOutput>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    let mut executor = TwoStageStrategyExecutor::new(
        state,
        authority.insecure_clone(),
        intent_id,
        ctx.intent_client.clone(),
        ctx.outbox_client.clone(),
        ctx.actions_callback_executor.clone(),
        execution_report,
    );

    let commit_signature = execute_with_timeout(
        time_left(),
        CommitStage {
            inner: &mut executor,
            transaction_preparator: &ctx.transaction_preparator,
            task_info_fetcher: &ctx.task_info_fetcher,
            committed_pubkeys,
        },
    )
    .await?;

    let mut finalize_executor = executor.done(commit_signature);
    let finalize_signature = execute_with_timeout(
        time_left(),
        FinalizeStage {
            inner: &mut finalize_executor,
            transaction_preparator: &ctx.transaction_preparator,
        },
    )
    .await?;

    let finalized_stage = finalize_executor.done(finalize_signature);
    Ok(ExecutionOutput::TwoStage {
        commit_signature: finalized_stage.commit_signature,
        finalize_signature: finalized_stage.finalize_signature,
    })
}
