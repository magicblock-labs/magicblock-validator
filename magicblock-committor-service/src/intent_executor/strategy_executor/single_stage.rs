use std::sync::Arc;

use magicblock_core::traits::{ActionError, ActionsCallbackScheduler};
use magicblock_program::outbox::ExecutionStage;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_signer::Signer;
use tracing::instrument;

use crate::{
    intent_executor::{
        error::{IntentExecutorError, IntentExecutorResult},
        intent_execution_client::IntentExecutionClient,
        strategy_executor::{
            patcher::SingleStagePatcher,
            utils::{
                handle_actions_result, stage_execution_loop, ExecutionState,
            },
        },
        IntentExecutionReport,
    },
    outbox::outbox_client::OutboxClient,
    tasks::{
        task_info_fetcher::{CacheTaskInfoFetcher, TaskInfoFetcher},
        task_strategist::TransactionStrategy,
    },
    transaction_preparator::TransactionPreparator,
};

pub struct SingleStageStrategyExecutor<'a, F, A, O> {
    current_attempt: u8,
    pending_signature: Option<Signature>,
    execution_report: &'a mut IntentExecutionReport,

    authority: Keypair,
    intent_id: u64,
    intent_client: IntentExecutionClient,
    task_info_fetcher: Arc<CacheTaskInfoFetcher<F>>,
    outbox_client: Arc<O>,
    callback_scheduler: A,
    transaction_strategy: TransactionStrategy,
}

impl<'a, F, A, O> SingleStageStrategyExecutor<'a, F, A, O>
where
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    pub fn new(
        authority: Keypair,
        intent_id: u64,
        intent_client: IntentExecutionClient,
        task_info_fetcher: Arc<CacheTaskInfoFetcher<F>>,
        outbox_client: Arc<O>,
        transaction_strategy: TransactionStrategy,
        callback_scheduler: A,
        execution_report: &'a mut IntentExecutionReport,
    ) -> Self {
        Self {
            authority,
            intent_id,
            pending_signature: None,
            intent_client,
            task_info_fetcher,
            outbox_client,
            transaction_strategy,
            callback_scheduler,
            execution_report,
            current_attempt: 0,
        }
    }

    #[instrument(
        skip(self, committed_pubkeys, transaction_preparator),
        fields(stage = "single_stage")
    )]
    pub async fn execute<T>(
        &mut self,
        committed_pubkeys: &[Pubkey],
        transaction_preparator: &T,
    ) -> IntentExecutorResult<Signature>
    where
        T: TransactionPreparator,
    {
        let patcher = SingleStagePatcher {
            authority: &self.authority,
            intent_client: &self.intent_client,
            callback_scheduler: &self.callback_scheduler,
            task_info_fetcher: &self.task_info_fetcher,
            committed_pubkeys,
            transaction_preparator,
        };
        let execution_state = ExecutionState {
            current_attempt: &mut self.current_attempt,
            transaction_strategy: &mut self.transaction_strategy,
            pending_signature: &mut self.pending_signature,
            execution_report: self.execution_report,
        };
        let result = stage_execution_loop(
            &self.authority,
            &self.intent_client,
            &*self.outbox_client,
            transaction_preparator,
            patcher,
            self.intent_id,
            ExecutionStage::SingleStage,
            IntentExecutorError::FailedFinalizePreparationError,
            execution_state,
        )
        .await?;

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
        signature: Option<Signature>,
        result: Result<(), impl Into<ActionError>>,
    ) {
        let junk_strategy = handle_actions_result(
            &self.authority.pubkey(),
            &self.callback_scheduler,
            self.execution_report,
            &mut self.transaction_strategy,
            signature,
            result.map_err(|err| err.into()),
        );
        self.execution_report.dispose(junk_strategy);
    }

    pub fn consume_strategy(self) -> TransactionStrategy {
        self.transaction_strategy
    }
}
