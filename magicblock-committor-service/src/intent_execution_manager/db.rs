use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
};

/// DB for storing intents that overflow committor channel
use async_trait::async_trait;
use magicblock_metrics::metrics;
use magicblock_program::magic_scheduled_base_intent::ScheduledIntentBundle;
use solana_pubkey::Pubkey;

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

    /// Returns the oldest (first stored) intent bundle
    async fn pop_intent_bundle(
        &self,
    ) -> DBResult<Option<ScheduledIntentBundle>>;
    fn is_empty(&self) -> bool;

    /// True when `intent` shares a committed pubkey with anything still queued.
    fn conflicts_with(&self, intent: &ScheduledIntentBundle) -> bool;
}

struct DummyDbInner {
    queue: VecDeque<ScheduledIntentBundle>,
    queued_pubkeys: HashMap<Pubkey, usize>,
}

impl DummyDbInner {
    fn track(&mut self, intent: &ScheduledIntentBundle) {
        for pubkey in intent.get_all_committed_pubkeys() {
            *self.queued_pubkeys.entry(pubkey).or_default() += 1;
        }
    }

    fn untrack(&mut self, intent: &ScheduledIntentBundle) {
        for pubkey in intent.get_all_committed_pubkeys() {
            if let Some(count) = self.queued_pubkeys.get_mut(&pubkey) {
                *count -= 1;
                if *count == 0 {
                    self.queued_pubkeys.remove(&pubkey);
                }
            }
        }
    }
}

pub(crate) struct DummyDB {
    db: Mutex<DummyDbInner>,
}

impl DummyDB {
    pub fn new() -> Self {
        Self {
            db: Mutex::new(DummyDbInner {
                queue: VecDeque::new(),
                queued_pubkeys: HashMap::new(),
            }),
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
        db.track(&intent_bundle);
        db.queue.push_back(intent_bundle);

        metrics::set_committor_intents_backlog_count(db.queue.len() as i64);
        Ok(())
    }

    async fn store_intent_bundles(
        &self,
        intent_bundles: Vec<ScheduledIntentBundle>,
    ) -> DBResult<()> {
        let mut db = self.db.lock().expect(POISONED_MUTEX_MSG);
        for intent in &intent_bundles {
            db.track(intent);
        }
        db.queue.extend(intent_bundles);

        metrics::set_committor_intents_backlog_count(db.queue.len() as i64);
        Ok(())
    }

    async fn pop_intent_bundle(
        &self,
    ) -> DBResult<Option<ScheduledIntentBundle>> {
        let mut db = self.db.lock().expect(POISONED_MUTEX_MSG);
        let res = db.queue.pop_front();
        if let Some(intent) = res.as_ref() {
            db.untrack(intent);
        }

        metrics::set_committor_intents_backlog_count(db.queue.len() as i64);
        Ok(res)
    }

    fn is_empty(&self) -> bool {
        self.db.lock().expect(POISONED_MUTEX_MSG).queue.is_empty()
    }

    fn conflicts_with(&self, intent: &ScheduledIntentBundle) -> bool {
        let db = self.db.lock().expect(POISONED_MUTEX_MSG);
        intent
            .get_all_committed_pubkeys()
            .iter()
            .any(|pubkey| db.queued_pubkeys.contains_key(pubkey))
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
