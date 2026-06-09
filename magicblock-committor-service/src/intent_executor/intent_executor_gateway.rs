use std::{sync::Arc, time::Duration};

use magicblock_core::traits::ActionsCallbackScheduler;
use solana_keypair::Keypair;

use crate::{
    intent_executor::{
        intent_execution_client::IntentExecutionClient,
        task_info_fetcher::{CacheTaskInfoFetcher, TaskInfoFetcher},
    },
    transaction_preparator::TransactionPreparator,
};

pub struct IntentExecutorGateway<T, F, A> {
    authority: Keypair,
    intent_client: IntentExecutionClient,
    transaction_preparator: T,
    task_info_fetcher: Arc<CacheTaskInfoFetcher<F>>,
    actions_callback_executor: A,
    /// Timeout for Intent's actions
    actions_timeout: Duration,
}

impl<T, F, A> IntentExecutorGateway<T, F, A>
where
    T: TransactionPreparator,
    F: TaskInfoFetcher,
    A: ActionsCallbackScheduler,
{
    pub fn new() -> Self {
        Self {}
    }
}

async fn intent_executor_gateway() {}
