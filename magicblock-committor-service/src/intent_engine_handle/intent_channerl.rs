use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use futures_util::ready;
use magicblock_program::outbox_intent_bundles::OutboxIntentBundle;
use pin_project::pin_project;
use tokio::sync::{
    mpsc,
    mpsc::{error::TrySendError, Receiver, Sender},
};
use tokio_stream::{wrappers::ReceiverStream, Stream};

use crate::intent_engine_handle::{db, db::DB};

const POISONED_MSG: &str = "Dummy DB mutex poisoned";

/// Handle for scheduling intents in ExecutionEngine
pub struct IntentScheduleHandle<D> {
    db: Arc<Mutex<D>>,
    sender: Sender<OutboxIntentBundle>,
}

impl<D: DB> IntentScheduleHandle<D> {
    pub fn new(db: Arc<Mutex<D>>, sender: Sender<OutboxIntentBundle>) -> Self {
        Self { db, sender }
    }

    pub fn schedule(
        &self,
        intent_bundles: Vec<OutboxIntentBundle>,
    ) -> Result<(), IntentScheduleError> {
        // If db not empty push el-t there
        // This means that at some point channel got full
        // Worker first will clean-up channel, and then DB.
        // Pushing into channel would break order of commits
        // Lock shall be held across to avoid races
        let db = self.db.lock().expect(POISONED_MSG);
        if !db.is_empty() {
            db.store_intent_bundles(intent_bundles)?;
            return Ok(());
        }

        let mut iter = intent_bundles.into_iter();
        // Treated as regular value not propagated lower
        #[allow(clippy::result_large_err)]
        let res = iter.try_for_each(|el| self.sender.try_send(el));
        match res {
            Ok(_) => Ok(()),
            Err(TrySendError::Closed(_)) => {
                Err(IntentScheduleError::ChannelClosed)
            }
            Err(TrySendError::Full(el)) => {
                let leftovers = std::iter::once(el).chain(iter).collect();
                db.store_intent_bundles(leftovers)
                    .map_err(IntentScheduleError::from)
            }
        }
    }
}

/// Stream of Intents that also handles backlog
/// If backlog is not empty we switch to reading from it until it is depleted
/// Once it is depleted we switch to polling `ReceiverStream`
#[pin_project]
pub struct IntentStream<D> {
    db: Arc<Mutex<D>>,
    #[pin]
    stream: ReceiverStream<OutboxIntentBundle>,
}

impl<D: DB> IntentStream<D> {
    pub fn new(
        db: Arc<Mutex<D>>,
        receiver: Receiver<OutboxIntentBundle>,
    ) -> Self {
        Self {
            db,
            stream: ReceiverStream::new(receiver),
        }
    }
}

impl<D: DB> Stream for IntentStream<D> {
    type Item = Result<OutboxIntentBundle, db::Error>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let this = self.project();
        let db = this.db.lock().expect(POISONED_MSG);
        // That means we have backlog
        // prior to using channel again we have to clean it all first
        if !db.is_empty() {
            // Some(T) always will be returned here as per check above
            let el = db.pop_intent_bundle();
            return Poll::Ready(el.transpose());
        }

        let item = ready!(this.stream.poll_next(cx));
        Poll::Ready(item.map(Ok))
    }
}

pub(crate) fn channel<D: DB>(
    db: &Arc<Mutex<D>>,
    buffer: usize,
) -> (IntentScheduleHandle<D>, IntentStream<D>) {
    let (sender, receiver) = mpsc::channel(buffer);

    let handle = IntentScheduleHandle::new(db.clone(), sender);
    let stream = IntentStream::new(db.clone(), receiver);

    (handle, stream)
}

#[derive(thiserror::Error, Debug)]
pub enum IntentScheduleError {
    #[error("Channel was closed")]
    ChannelClosed,
    #[error("DBError: {0}")]
    DBError(#[from] db::Error),
}
