use std::{collections::VecDeque, sync::Mutex};

/// DB for storing messages that overflow committor channel
use async_trait::async_trait;
use magicblock_program::magic_scheduled_l1_message::ScheduledL1Message;

const POISONED_MUTEX_MSG: &str = "Mutex poisoned";

#[async_trait]
pub trait DB: Send + Sync + 'static {
    async fn store_l1_message(
        &self,
        l1_message: ScheduledL1Message,
    ) -> DBResult<()>;
    async fn store_l1_messages(
        &self,
        l1_messages: Vec<ScheduledL1Message>,
    ) -> DBResult<()>;
    /// Return message with smallest bundle_id
    async fn pop_l1_message(&self) -> DBResult<Option<ScheduledL1Message>>;
    fn is_empty(&self) -> bool;
}

pub(crate) struct DummyDB {
    db: Mutex<VecDeque<ScheduledL1Message>>,
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
    async fn store_l1_message(
        &self,
        l1_message: ScheduledL1Message,
    ) -> DBResult<()> {
        self.db
            .lock()
            .expect(POISONED_MUTEX_MSG)
            .push_back(l1_message);
        Ok(())
    }

    async fn store_l1_messages(
        &self,
        l1_messages: Vec<ScheduledL1Message>,
    ) -> DBResult<()> {
        self.db
            .lock()
            .expect(POISONED_MUTEX_MSG)
            .extend(l1_messages.into_iter());
        Ok(())
    }

    async fn pop_l1_message(&self) -> DBResult<Option<ScheduledL1Message>> {
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
