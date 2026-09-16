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
    mpsc::{Receiver, Sender},
};
use tokio_stream::{Stream, wrappers::ReceiverStream};

use crate::intent_engine::{db, db::BacklogDB};

const POISONED_MSG: &str = "intent backlog mutex poisoned";

/// Stream of Intents that also handles backlog
/// If backlog is not empty we switch to reading from it until it is depleted
/// Once it is depleted we switch to polling `ReceiverStream`
#[pin_project]
pub struct IntentStream<D> {
    db: Arc<Mutex<D>>,
    #[pin]
    stream: ReceiverStream<OutboxIntentBundle>,
}

impl<D: BacklogDB> IntentStream<D> {
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

impl<D: BacklogDB> Stream for IntentStream<D> {
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
            // Before starting to clean backlog we need to clean channel first.
            // A closed channel (`Ready(None)`) must NOT end the stream here -
            // backlog still has items to drain, and the channel closing
            // doesn't mean there's no more work left.
            if let Poll::Ready(Some(item)) = this.stream.poll_next(cx) {
                Poll::Ready(Some(Ok(item)))
            } else {
                // Some(T) always will be returned here as per check above
                let el = db.pop_intent_bundle();
                Poll::Ready(el.transpose())
            }
        } else {
            let item = ready!(this.stream.poll_next(cx));
            Poll::Ready(item.map(Ok))
        }
    }
}

pub(crate) fn channel<D: BacklogDB>(
    db: &Arc<Mutex<D>>,
    buffer: usize,
) -> (Sender<OutboxIntentBundle>, IntentStream<D>) {
    let (sender, receiver) = mpsc::channel(buffer);

    let stream = IntentStream::new(db.clone(), receiver);

    (sender, stream)
}

#[derive(thiserror::Error, Debug)]
pub enum IntentScheduleError {
    #[error("Channel was closed")]
    ChannelClosed,
    #[error("DBError: {0}")]
    DBError(#[from] db::Error),
}
