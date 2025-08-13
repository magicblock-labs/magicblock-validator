use std::{collections::VecDeque, sync::Mutex};

/// DB for storing intents that overflow committor channel
use async_trait::async_trait;

use crate::types::ScheduledBaseIntentWrapper;

const POISONED_MUTEX_MSG: &str = "Dummy db mutex poisoned";

#[async_trait]
pub trait DB: Send + Sync + 'static {
    async fn store_base_intent(
        &self,
        base_intent: ScheduledBaseIntentWrapper,
    ) -> DBResult<()>;
    async fn store_base_intents(
        &self,
        base_intents: Vec<ScheduledBaseIntentWrapper>,
    ) -> DBResult<()>;

    /// Returns intent with smallest id
    async fn pop_base_intent(
        &self,
    ) -> DBResult<Option<ScheduledBaseIntentWrapper>>;
    fn is_empty(&self) -> bool;
}

pub(crate) struct DummyDB {
    db: Mutex<VecDeque<ScheduledBaseIntentWrapper>>,
}

impl DummyDB {
    pub fn new() -> Self {
        Self {
            db: Mutex::new(VecDeque::new()),
        }
    }
}

#[async_trait]
impl DB for DummyDB {
    async fn store_base_intent(
        &self,
        base_intent: ScheduledBaseIntentWrapper,
    ) -> DBResult<()> {
        self.db
            .lock()
            .expect(POISONED_MUTEX_MSG)
            .push_back(base_intent);
        Ok(())
    }

    async fn store_base_intents(
        &self,
        base_intents: Vec<ScheduledBaseIntentWrapper>,
    ) -> DBResult<()> {
        self.db
            .lock()
            .expect(POISONED_MUTEX_MSG)
            .extend(base_intents.into_iter());
        Ok(())
    }

    async fn pop_base_intent(
        &self,
    ) -> DBResult<Option<ScheduledBaseIntentWrapper>> {
        Ok(self.db.lock().expect(POISONED_MUTEX_MSG).pop_front())
    }

    fn is_empty(&self) -> bool {
        self.db.lock().expect(POISONED_MUTEX_MSG).is_empty()
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("StoreError: {0}")]
    StoreError(anyhow::Error),
    #[error("FetchError: {0}")]
    FetchError(anyhow::Error),
}

pub type DBResult<T, E = Error> = Result<T, E>;
