use std::ops::ControlFlow;

use async_trait::async_trait;

use crate::{
    intent_executor::error::{
        IntentExecutorResult, TransactionStrategyExecutionError,
    },
    tasks::task_strategist::TransactionStrategy,
};

#[async_trait]
pub(in crate::intent_executor) trait Patcher {
    async fn patch(
        &mut self,
        err: &TransactionStrategyExecutionError,
        strategy: &mut TransactionStrategy,
    ) -> IntentExecutorResult<ControlFlow<(), TransactionStrategy>>;
}
