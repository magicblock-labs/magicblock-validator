use std::{collections::VecDeque, sync::Mutex};
use std::sync::Arc;
/// DB for storing intents that overflow committor channel
use async_trait::async_trait;
use solana_account::ReadableAccount;
use magicblock_accounts_db::AccountsDb;
use magicblock_accounts_db::traits::AccountsBank;
use magicblock_core::intent::outbox::outbox_intent_pda;
use magicblock_metrics::metrics;
use magicblock_program::magic_scheduled_base_intent::ScheduledIntentBundle;
use magicblock_program::outbox_intent_bundles::OutboxIntentBundle;

const POISONED_MUTEX_MSG: &str = "Dummy db mutex poisoned";

#[async_trait]
pub trait DB: Send + Sync + 'static {
    async fn store_intent_bundle(
        &self,
        intent_bundle: OutboxIntentBundle,
    ) -> DBResult<()>;
    async fn store_intent_bundles(
        &self,
        intent_bundles: Vec<OutboxIntentBundle>,
    ) -> DBResult<()>;

    /// Returns the oldest (first stored) intent bundle
    async fn pop_intent_bundle(
        &self,
    ) -> DBResult<Option<OutboxIntentBundle>>;
    fn is_empty(&self) -> bool;
}

pub(crate) struct DummyDB {
    db: Mutex<VecDeque<OutboxIntentBundle>>,
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
        intent_bundle: OutboxIntentBundle,
    ) -> DBResult<()> {
        let mut db = self.db.lock().expect(POISONED_MUTEX_MSG);
        db.push_back(intent_bundle);

        metrics::set_committor_intents_backlog_count(db.len() as i64);
        Ok(())
    }

    async fn store_intent_bundles(
        &self,
        intent_bundles: Vec<OutboxIntentBundle>,
    ) -> DBResult<()> {
        let mut db = self.db.lock().expect(POISONED_MUTEX_MSG);
        db.extend(intent_bundles);

        metrics::set_committor_intents_backlog_count(db.len() as i64);
        Ok(())
    }

    async fn pop_intent_bundle(
        &self,
    ) -> DBResult<Option<OutboxIntentBundle>> {
        let mut db = self.db.lock().expect(POISONED_MUTEX_MSG);
        let res = db.pop_front();

        metrics::set_committor_intents_backlog_count(db.len() as i64);
        Ok(res)
    }

    fn is_empty(&self) -> bool {
        self.db.lock().expect(POISONED_MUTEX_MSG).is_empty()
    }
}

pub struct DumberDB {
    accounts_db: Arc<AccountsDb>,
    queue: Mutex<VecDeque<u64>>
}

impl DumberDB {
    pub fn new(accounts_db: Arc<AccountsDb>) -> Self {
        Self {
            accounts_db,
            queue: Mutex::new(VecDeque::new()),
        }
    }
}

#[async_trait]
impl DB for DumberDB {
    async fn store_intent_bundle(
        &self,
        intent_bundle: OutboxIntentBundle,
    ) -> DBResult<()> {
        let mut queue = self.queue.lock().expect(POISONED_MUTEX_MSG);
        queue.push_back(intent_bundle.inner.id);

        metrics::set_committor_intents_backlog_count(queue.len() as i64);
        Ok(())
    }

    async fn store_intent_bundles(
        &self,
        intent_bundles: Vec<OutboxIntentBundle>,
    ) -> DBResult<()> {
        let mut queue = self.queue.lock().expect(POISONED_MUTEX_MSG);
        queue.extend(intent_bundles.into_iter().map(|el| el.inner.id));

        metrics::set_committor_intents_backlog_count(queue.len() as i64);
        Ok(())
    }

    async fn pop_intent_bundle(
        &self,
    ) -> DBResult<Option<OutboxIntentBundle>> {
        let mut queue = self.queue.lock().expect(POISONED_MUTEX_MSG);
        let Some(id) = queue.pop_front() else {
            return Ok(None);
        };
        metrics::set_committor_intents_backlog_count(queue.len() as i64);
        drop(queue);

        let intent_pda = outbox_intent_pda(id);
        let account = self.accounts_db.get_account(&intent_pda).ok_or(Error::IntentNotFoundError(id))?;

        let outbox_intent = OutboxIntentBundle::try_from_bytes(account.data())?;
        Ok(Some(outbox_intent))
    }

    fn is_empty(&self) -> bool {
        self.queue.lock().expect(POISONED_MUTEX_MSG).is_empty()
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Failed to find intent in AccountsDB, id: {0}")]
    IntentNotFoundError(u64),
    #[error("Failed to deserialize Outbox Account")]
    DeserializeError(#[from] bincode::Error)
}

pub type DBResult<T, E = Error> = Result<T, E>;
