use std::{collections::VecDeque, sync::Mutex};

/// DB for storing intents that overflow committor channel
use async_trait::async_trait;
use magicblock_metrics::metrics;
use magicblock_program::magic_scheduled_base_intent::ScheduledIntentBundle;

const POISONED_MUTEX_MSG: &str = "Dummy db mutex poisoned";

#[async_trait]
pub trait DB: Send + Sync + 'static {
    async fn store_intent_bundle(
        &self,
        intent_bundle: ScheduledIntentBundle,
    ) -> DBResult<()>;
    async fn store_intent_bundles(
        &self,
        intent_bundles: Vec<ScheduledIntentBundle>,
    ) -> DBResult<()>;

    /// Returns intent with smallest id
    async fn pop_intent_bundle(
        &self,
    ) -> DBResult<Option<ScheduledIntentBundle>>;
    fn is_empty(&self) -> bool;
}

pub(crate) struct DummyDB {
    db: Mutex<VecDeque<ScheduledIntentBundle>>,
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
    async fn store_intent_bundle(
        &self,
        intent_bundle: ScheduledIntentBundle,
    ) -> DBResult<()> {
        let mut db = self.db.lock().expect(POISONED_MUTEX_MSG);
        db.push_back(intent_bundle);

        metrics::set_committor_intents_backlog_count(db.len() as i64);
        Ok(())
    }

    async fn store_intent_bundles(
        &self,
        intent_bundles: Vec<ScheduledIntentBundle>,
    ) -> DBResult<()> {
        let mut db = self.db.lock().expect(POISONED_MUTEX_MSG);
        db.extend(intent_bundles);

        metrics::set_committor_intents_backlog_count(db.len() as i64);
        Ok(())
    }

    async fn pop_intent_bundle(
        &self,
    ) -> DBResult<Option<ScheduledIntentBundle>> {
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
