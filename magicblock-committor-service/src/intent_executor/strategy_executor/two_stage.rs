use std::{mem, sync::Arc};

use magicblock_core::traits::{ActionError, ActionsCallbackScheduler};
use magicblock_program::outbox::{ExecutionStage, TwoStageProgress};
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
            patcher::{CommitStagePatcher, FinalizeStagePatcher},
            two_stage::sealed::Sealed,
            utils::{
                handle_actions_result, stage_execution_loop, ExecutionState,
            },
        },
        IntentExecutionReport,
    },
    outbox_client::OutboxClient,
    tasks::{
        task_info_fetcher::{CacheTaskInfoFetcher, TaskInfoFetcher},
        task_strategist::TransactionStrategy,
    },
    transaction_preparator::TransactionPreparator,
};

pub struct Initialized {
    /// Commit stage strategy
    commit_strategy: TransactionStrategy,
    /// Finalize stage strategy
    finalize_strategy: TransactionStrategy,
    /// Signature pending confirmation
    /// Sources: Outbox with status Committing, timeout
    pending_signature: Option<Signature>,
    current_attempt: u8,
}

impl Initialized {
    pub fn new(
        commit_strategy: TransactionStrategy,
        finalize_strategy: TransactionStrategy,
    ) -> Self {
        Self {
            commit_strategy,
            finalize_strategy,
            pending_signature: None,
            current_attempt: 0,
        }
    }
}

pub struct Committed {
    /// Signature of commit stage
    commit_signature: Signature,
    /// Finalize stage strategy
    finalize_strategy: TransactionStrategy,
    /// Signature pending confirmation
    /// Sources: Outbox with status Finalizing, timeout
    pending_signature: Option<Signature>,
    current_attempt: u8,
}

impl Committed {
    pub fn new(
        commit_signature: Signature,
        finalize_strategy: TransactionStrategy,
    ) -> Self {
        Self {
            commit_signature,
            finalize_strategy,
            pending_signature: None,
            current_attempt: 0,
        }
    }
}

pub struct Finalized {
    /// Signature of commit stage
    pub commit_signature: Signature,
    /// Signature of finalize stage
    pub finalize_signature: Signature,
}

pub struct TwoStageStrategyExecutor<'a, A, O, S: Sealed> {
    /// State per stage
    state: S,
    /// Common state across the stages
    authority: Keypair,
    intent_id: u64,
    intent_client: IntentExecutionClient,
    outbox_client: Arc<O>,
    callback_scheduler: A,
    execution_report: &'a mut IntentExecutionReport,
}

impl<'a, A, O> TwoStageStrategyExecutor<'a, A, O, Initialized>
where
    A: ActionsCallbackScheduler,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    pub fn new(
        state: Initialized,
        authority: Keypair,
        intent_id: u64,
        intent_client: IntentExecutionClient,
        outbox_client: Arc<O>,
        callback_scheduler: A,
        execution_report: &'a mut IntentExecutionReport,
    ) -> Self {
        Self {
            authority,
            intent_id,
            intent_client,
            execution_report,
            callback_scheduler,
            outbox_client,
            state,
        }
    }

    #[instrument(
        skip(
            self,
            committed_pubkeys,
            transaction_preparator,
            task_info_fetcher,
        ),
        fields(stage = "commit")
    )]
    pub async fn commit<T, F>(
        &mut self,
        committed_pubkeys: &[Pubkey],
        transaction_preparator: &T,
        task_info_fetcher: &CacheTaskInfoFetcher<F>,
    ) -> IntentExecutorResult<Signature>
    where
        T: TransactionPreparator,
        F: TaskInfoFetcher,
    {
        let execution_state = ExecutionState {
            current_attempt: &mut self.state.current_attempt,
            transaction_strategy: &mut self.state.commit_strategy,
            pending_signature: &mut self.state.pending_signature,
            execution_report: self.execution_report,
        };
        let commit_stage_patcher = CommitStagePatcher {
            authority: &self.authority,
            intent_client: &self.intent_client,
            callback_scheduler: &self.callback_scheduler,
            task_info_fetcher,
            committed_pubkeys,
            transaction_preparator,
        };

        // Execute commit stage
        let commit_result = stage_execution_loop(
            &self.authority,
            &self.intent_client,
            &*self.outbox_client,
            transaction_preparator,
            commit_stage_patcher,
            self.intent_id,
            |sig| ExecutionStage::TwoStage(TwoStageProgress::Committing(sig)),
            IntentExecutorError::FailedCommitPreparationError,
            execution_state,
        )
        .await?;
        self.execute_callbacks(
            commit_result.as_ref().ok().copied(),
            commit_result.as_ref().map(|_| ()),
        );
        self.execution_report
            .dispose(self.state.commit_strategy.clone());
        if commit_result.is_err() {
            self.execution_report
                .dispose(mem::take(&mut self.state.finalize_strategy));
        }
        commit_result.map_err(|err| {
            IntentExecutorError::from_commit_execution_error(err)
        })
    }

    pub fn has_callbacks(&self) -> bool {
        self.state.commit_strategy.has_actions_callbacks()
            || self.state.finalize_strategy.has_actions_callbacks()
    }

    /// On `Err`: removes actions from both commit and finalize strategies and
    /// executes all their callbacks with the error.
    /// On `Ok`: removes actions only from commit strategy and executes their
    /// callbacks, preserving finalize-stage actions for the finalize phase.
    pub fn execute_callbacks(
        &mut self,
        signature: Option<Signature>,
        result: Result<(), impl Into<ActionError>>,
    ) {
        let result = result.map(|_| ()).map_err(|err| err.into());
        let junk_strategy = handle_actions_result(
            &self.authority.pubkey(),
            &self.callback_scheduler,
            self.execution_report,
            &mut self.state.commit_strategy,
            signature,
            result.clone(),
        );
        self.execution_report.dispose(junk_strategy);

        if result.is_err() {
            let junk_strategy = handle_actions_result(
                &self.authority.pubkey(),
                &self.callback_scheduler,
                self.execution_report,
                &mut self.state.finalize_strategy,
                signature,
                result,
            );
            self.execution_report.dispose(junk_strategy);
        }
    }

    /// Transitions to next executor state
    pub fn done(
        #[allow(unused_mut)] mut self,
        commit_signature: Signature,
    ) -> TwoStageStrategyExecutor<'a, A, O, Committed> {
        #[cfg(feature = "dev-context-only-utils")]
        self.execution_report
            .add_succeeded_transaction_strategy(mem::take(
                &mut self.state.commit_strategy,
            ));

        TwoStageStrategyExecutor {
            authority: self.authority,
            intent_id: self.intent_id,
            intent_client: self.intent_client,
            outbox_client: self.outbox_client,
            callback_scheduler: self.callback_scheduler,
            execution_report: self.execution_report,
            state: Committed {
                commit_signature,
                finalize_strategy: self.state.finalize_strategy,
                pending_signature: None,
                current_attempt: 0,
            },
        }
    }
}

impl<'a, A, O> TwoStageStrategyExecutor<'a, A, O, Committed>
where
    A: ActionsCallbackScheduler,
    O: OutboxClient,
    O::Error: Into<IntentExecutorError>,
{
    pub fn committed(
        state: Committed,
        authority: Keypair,
        intent_id: u64,
        intent_client: IntentExecutionClient,
        outbox_client: Arc<O>,
        callback_scheduler: A,
        execution_report: &'a mut IntentExecutionReport,
    ) -> Self {
        Self {
            authority,
            intent_id,
            intent_client,
            execution_report,
            callback_scheduler,
            outbox_client,
            state,
        }
    }

    #[instrument(
        skip(self, transaction_preparator),
        fields(stage = "finalize")
    )]
    pub async fn finalize<T>(
        &mut self,
        transaction_preparator: &T,
    ) -> IntentExecutorResult<Signature>
    where
        T: TransactionPreparator,
    {
        let commit_signature = self.state.commit_signature;
        let execution_state = ExecutionState {
            current_attempt: &mut self.state.current_attempt,
            transaction_strategy: &mut self.state.finalize_strategy,
            pending_signature: &mut self.state.pending_signature,
            execution_report: self.execution_report,
        };
        let finalize_stage_patcher = FinalizeStagePatcher {
            authority: &self.authority,
            callback_scheduler: &self.callback_scheduler,
        };

        // Execute finalize stage
        let finalize_result = stage_execution_loop(
            &self.authority,
            &self.intent_client,
            &*self.outbox_client,
            transaction_preparator,
            finalize_stage_patcher,
            self.intent_id,
            |sig| {
                ExecutionStage::TwoStage(TwoStageProgress::Finalizing {
                    commit: commit_signature,
                    finalize: sig,
                })
            },
            IntentExecutorError::FailedFinalizePreparationError,
            execution_state,
        )
        .await?;
        // Even if failed - dump finalize into junk
        self.execute_callbacks(
            finalize_result.as_ref().ok().copied(),
            finalize_result.as_ref().map(|_| ()),
        );
        self.execution_report
            .dispose(self.state.finalize_strategy.clone());
        finalize_result.map_err(|err| {
            IntentExecutorError::from_finalize_execution_error(
                err,
                Some(self.state.commit_signature),
            )
        })
    }

    pub fn has_callbacks(&self) -> bool {
        self.state.finalize_strategy.has_actions_callbacks()
    }

    /// Removes actions from finalize strategy
    /// Executes callbacks
    pub fn execute_callbacks(
        &mut self,
        signature: Option<Signature>,
        result: Result<(), impl Into<ActionError>>,
    ) {
        let junk_strategy = handle_actions_result(
            &self.authority.pubkey(),
            &self.callback_scheduler,
            self.execution_report,
            &mut self.state.finalize_strategy,
            signature,
            result.map_err(|err| err.into()),
        );
        self.execution_report.dispose(junk_strategy);
    }

    /// Transitions to next executor state
    #[allow(unused_mut)]
    pub fn done(mut self, finalize_signature: Signature) -> Finalized {
        #[cfg(feature = "dev-context-only-utils")]
        self.execution_report
            .add_succeeded_transaction_strategy(mem::take(
                &mut self.state.finalize_strategy,
            ));

        Finalized {
            commit_signature: self.state.commit_signature,
            finalize_signature,
        }
    }
}

mod sealed {
    pub trait Sealed {}

    impl Sealed for super::Initialized {}
    impl Sealed for super::Committed {}
    impl Sealed for super::Finalized {}
}
