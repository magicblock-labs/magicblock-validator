use std::{collections::VecDeque, sync::Mutex};

/// DB for storing intents that overflow committor channel
use async_trait::async_trait;
use magicblock_metrics::metrics;

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
        let mut db = self.db.lock().expect(POISONED_MUTEX_MSG);
        db.push_back(base_intent);

        metrics::set_committor_intents_backlog_count(db.len() as i64);
        Ok(())
    }

    async fn store_base_intents(
        &self,
        base_intents: Vec<ScheduledBaseIntentWrapper>,
    ) -> DBResult<()> {
        let mut db = self.db.lock().expect(POISONED_MUTEX_MSG);
        db.extend(base_intents.into_iter());

        metrics::set_committor_intents_backlog_count(db.len() as i64);
        Ok(())
    }

    async fn pop_base_intent(
        &self,
    ) -> DBResult<Option<ScheduledBaseIntentWrapper>> {
        let mut db = self.db.lock().expect(POISONED_MUTEX_MSG);
        let res = db.pop_front();

        metrics::set_committor_intents_backlog_count(db.len() as i64);
        Ok(res)
    }

    fn is_empty(&self) -> bool {
        self.db.lock().expect(POISONED_MUTEX_MSG).is_empty()
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("StoreError")]
    StoreError,
    #[error("FetchError")]
    FetchError,
}

pub type DBResult<T, E = Error> = Result<T, E>;
