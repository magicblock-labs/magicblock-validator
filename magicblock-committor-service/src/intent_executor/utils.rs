use std::{sync::Arc, time::Duration};

use futures_util::future::join;
use magicblock_core::traits::ActionsCallbackScheduler;
use magicblock_program::magic_scheduled_base_intent::ScheduledIntentBundle;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;

use crate::{
    intent_executor::{
        error::{IntentExecutorError, IntentExecutorResult},
        strategy_executor::{
            single_stage::SingleStageStrategyExecutor,
            two_stage,
            two_stage::TwoStageStrategyExecutor,
            utils::{
                execute_with_timeout, handle_cpi_limit_error, CommitStage,
                FinalizeStage, SingleStage,
            },
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

pub(in crate::intent_executor) async fn execute_single_stage_flow<T, F, A, O>(
    ctx: &IntentExecutorCtx<T, F, A, O>,
    authority: &Keypair,
    intent_bundle: ScheduledIntentBundle,
    pending_signature: Option<Signature>,
    transaction_strategy: TransactionStrategy,
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
    let committed_pubkeys = intent_bundle.get_all_committed_pubkeys();

    let mut single_stage_executor = SingleStageStrategyExecutor::new(
        authority.insecure_clone(),
        intent_bundle.id,
        pending_signature,
        ctx.intent_client.clone(),
        ctx.task_info_fetcher.clone(),
        ctx.outbox_client.clone(),
        transaction_strategy,
        ctx.actions_callback_executor.clone(),
        execution_report,
    );
    let res = execute_with_timeout(
        time_left(),
        SingleStage {
            inner: &mut single_stage_executor,
            transaction_preparator: &ctx.transaction_preparator,
            committed_pubkeys: &committed_pubkeys,
        },
    )
    .await;

    // Here we continue only IF the error is a limit-type execution error
    // We can recover that Error by splitting execution
    // in 2 stages - commit & finalize
    // Otherwise we return error
    let execution_err = match res {
        Err(IntentExecutorError::FailedToFinalizeError {
            err,
            commit_signature: _,
            finalize_signature: _,
        }) if !committed_pubkeys.is_empty()
            && err.is_recoverable_by_two_stage() =>
        {
            err
        }
        res => {
            let signature = res.as_ref().ok().copied();
            single_stage_executor
                .execute_callbacks(signature, res.as_ref().map(|_| ()));
            let transaction_strategy = single_stage_executor.consume_strategy();
            execution_report.dispose(transaction_strategy);
            return res.map(ExecutionOutput::SingleStage);
        }
    };

    // With actions, we can't predict num of CPIs
    // If we get here we will try to switch from Single stage to Two Stage commit
    // Note that this not necessarily will pass at the end due to the same reason
    let strategy = single_stage_executor.consume_strategy();
    let (commit_strategy, finalize_strategy, cleanup) =
        handle_cpi_limit_error(&authority.pubkey(), strategy);
    execution_report.dispose(cleanup);
    execution_report.add_patched_error(execution_err);

    // Create state for two stage flow
    // Pending signature set to None as if we're here tx failed
    let state =
        two_stage::Initialized::new(commit_strategy, finalize_strategy, None);

    execute_two_stage_flow(
        ctx,
        state,
        authority,
        intent_bundle,
        execution_report,
        time_left,
    )
    .await
}

pub(in crate::intent_executor) async fn execute_two_stage_flow<T, F, A, O>(
    ctx: &IntentExecutorCtx<T, F, A, O>,
    state: two_stage::Initialized,
    authority: &Keypair,
    intent_bundle: ScheduledIntentBundle,
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
    let committed_pubkeys = intent_bundle.get_all_committed_pubkeys();
    let mut executor = TwoStageStrategyExecutor::new(
        state,
        authority.insecure_clone(),
        intent_bundle.id,
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
            committed_pubkeys: &committed_pubkeys,
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
