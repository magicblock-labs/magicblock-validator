use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures_util::ready;
use parking_lot::Mutex;
use pin_project::pin_project;
use program::outbox_intent_bundles::OutboxIntentBundle;
use tokio::sync::mpsc::Receiver;
use tokio_stream::{Stream, wrappers::ReceiverStream};

use crate::execution::backlog::{self, BacklogDB};

/// Stream of intents from the live channel and persisted backlog.
///
/// When backlog exists, already-buffered channel entries are returned first
/// because they were scheduled before the backlog entries. If the channel has
/// nothing ready, the stream immediately pops from backlog instead of waiting.
#[pin_project]
pub(crate) struct IntentStream {
    backlog: Arc<Mutex<dyn BacklogDB>>,
    #[pin]
    stream: ReceiverStream<OutboxIntentBundle>,
}

impl IntentStream {
    pub(crate) fn new(
        backlog: Arc<Mutex<dyn BacklogDB>>,
        receiver: Receiver<OutboxIntentBundle>,
    ) -> Self {
        Self {
            backlog,
            stream: ReceiverStream::new(receiver),
        }
    }
}

impl Stream for IntentStream {
    type Item = Result<OutboxIntentBundle, backlog::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        let backlog = this.backlog.lock();
        if backlog.is_empty() {
            let item = ready!(this.stream.poll_next(cx));
            Poll::Ready(item.map(Ok))
        } else {
            // A closed channel (`Ready(None)`) must not end the stream here:
            // backlog still has work to drain.
            if let Poll::Ready(Some(item)) = this.stream.poll_next(cx) {
                Poll::Ready(Some(Ok(item)))
            } else {
                // Some(T) is expected because emptiness was checked above.
                Poll::Ready(backlog.pop_intent_bundle().transpose())
            }
        }
    }
}
