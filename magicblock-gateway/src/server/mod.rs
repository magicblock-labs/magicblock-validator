use std::sync::Arc;

use tokio::sync::oneshot::{self, Receiver, Sender};

pub(crate) mod http;
pub(crate) mod websocket;

struct Shutdown(Option<Sender<()>>);

impl Shutdown {
    fn new() -> (Arc<Self>, Receiver<()>) {
        let (tx, rx) = oneshot::channel();
        (Self(Some(tx)).into(), rx)
    }
}

impl Drop for Shutdown {
    fn drop(&mut self) {
        self.0.take().map(|tx| tx.send(()));
    }
}
