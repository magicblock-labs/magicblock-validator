use std::{
    cell::RefCell,
    collections::VecDeque,
    sync::{Arc, Mutex},
};

/// DB for storing intents that overflow committor channel
use magicblock_accounts_db::{traits::AccountsBank, AccountsDb};
use magicblock_core::intent::outbox::outbox_intent_pda;
use magicblock_metrics::metrics;
use magicblock_program::outbox_intent_bundles::OutboxIntentBundle;
use solana_account::ReadableAccount;

const POISONED_MUTEX_MSG: &str = "Dummy db mutex poisoned";

pub trait DB: Send + 'static {
    fn store_intent_bundle(
        &self,
        intent_bundle: OutboxIntentBundle,
    ) -> DBResult<()>;
    fn store_intent_bundles(
        &self,
        intent_bundles: Vec<OutboxIntentBundle>,
    ) -> DBResult<()>;

    /// Returns the oldest (first stored) intent bundle
    fn pop_intent_bundle(&self) -> DBResult<Option<OutboxIntentBundle>>;
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

impl DB for DummyDB {
    fn store_intent_bundle(
        &self,
        intent_bundle: OutboxIntentBundle,
    ) -> DBResult<()> {
        let mut db = self.db.lock().expect(POISONED_MUTEX_MSG);
        db.push_back(intent_bundle);

        metrics::set_committor_intents_backlog_count(db.len() as i64);
        Ok(())
    }

    fn store_intent_bundles(
        &self,
        intent_bundles: Vec<OutboxIntentBundle>,
    ) -> DBResult<()> {
        let mut db = self.db.lock().expect(POISONED_MUTEX_MSG);
        db.extend(intent_bundles);

        metrics::set_committor_intents_backlog_count(db.len() as i64);
        Ok(())
    }

    fn pop_intent_bundle(&self) -> DBResult<Option<OutboxIntentBundle>> {
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
    queue: RefCell<VecDeque<u64>>,
}

impl DumberDB {
    pub fn new(accounts_db: Arc<AccountsDb>) -> Self {
        Self {
            accounts_db,
            queue: RefCell::new(VecDeque::new()),
        }
    }
}

impl DB for DumberDB {
    fn store_intent_bundle(
        &self,
        intent_bundle: OutboxIntentBundle,
    ) -> DBResult<()> {
        let mut queue = self.queue.borrow_mut();
        queue.push_back(intent_bundle.inner.id);

        metrics::set_committor_intents_backlog_count(queue.len() as i64);
        Ok(())
    }

    fn store_intent_bundles(
        &self,
        intent_bundles: Vec<OutboxIntentBundle>,
    ) -> DBResult<()> {
        let mut queue = self.queue.borrow_mut();
        queue.extend(intent_bundles.into_iter().map(|el| el.inner.id));

        metrics::set_committor_intents_backlog_count(queue.len() as i64);
        Ok(())
    }

    fn pop_intent_bundle(&self) -> DBResult<Option<OutboxIntentBundle>> {
        let mut queue = self.queue.borrow_mut();
        let Some(id) = queue.pop_front() else {
            return Ok(None);
        };
        metrics::set_committor_intents_backlog_count(queue.len() as i64);
        drop(queue);

        let intent_pda = outbox_intent_pda(id);
        let account = self
            .accounts_db
            .get_account(&intent_pda)
            .ok_or(Error::IntentNotFoundError(id))?;

        let outbox_intent = OutboxIntentBundle::try_from_bytes(account.data())?;
        Ok(Some(outbox_intent))
    }

    fn is_empty(&self) -> bool {
        self.queue.borrow().is_empty()
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Failed to find intent in AccountsDB, id: {0}")]
    IntentNotFoundError(u64),
    #[error("Failed to deserialize Outbox Account")]
    DeserializeError(#[from] bincode::Error),
}

pub type DBResult<T, E = Error> = Result<T, E>;
