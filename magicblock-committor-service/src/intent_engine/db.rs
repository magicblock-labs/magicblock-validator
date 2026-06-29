use std::{cell::RefCell, collections::VecDeque, sync::Arc};

/// DB for storing intents that overflow committor channel
use magicblock_accounts_db::{traits::AccountsBank, AccountsDb};
use magicblock_core::intent::outbox::outbox_intent_pda;
use magicblock_metrics::metrics;
use magicblock_program::outbox_intent_bundles::OutboxIntentBundle;
use solana_account::ReadableAccount;

pub trait BacklogDB: Send + 'static {
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

pub struct DummyIntentBacklog {
    accounts_db: Arc<AccountsDb>,
    queue: RefCell<VecDeque<u64>>,
}

impl DummyIntentBacklog {
    pub fn new(accounts_db: Arc<AccountsDb>) -> Self {
        Self {
            accounts_db,
            queue: RefCell::new(VecDeque::new()),
        }
    }
}

impl BacklogDB for DummyIntentBacklog {
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

#[cfg(any(test, feature = "dev-context-only-utils"))]
pub struct DummyDB {
    db: std::sync::Mutex<VecDeque<OutboxIntentBundle>>,
}

#[cfg(any(test, feature = "dev-context-only-utils"))]
impl Default for DummyDB {
    fn default() -> Self {
        Self {
            db: std::sync::Mutex::new(VecDeque::new()),
        }
    }
}

#[cfg(any(test, feature = "dev-context-only-utils"))]
impl DummyDB {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(any(test, feature = "dev-context-only-utils"))]
impl BacklogDB for DummyDB {
    fn store_intent_bundle(
        &self,
        intent_bundle: OutboxIntentBundle,
    ) -> DBResult<()> {
        self.db.lock().unwrap().push_back(intent_bundle);
        Ok(())
    }

    fn store_intent_bundles(
        &self,
        intent_bundles: Vec<OutboxIntentBundle>,
    ) -> DBResult<()> {
        self.db.lock().unwrap().extend(intent_bundles);
        Ok(())
    }

    fn pop_intent_bundle(&self) -> DBResult<Option<OutboxIntentBundle>> {
        Ok(self.db.lock().unwrap().pop_front())
    }

    fn is_empty(&self) -> bool {
        self.db.lock().unwrap().is_empty()
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
