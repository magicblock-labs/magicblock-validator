use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use solana_pubkey::Pubkey;
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot},
};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use url::Url;

use super::{Account, LOOPBACK, RPC_VERSION, Tasks, rpc_result, within};

/// Held subscription request; the scenario decides when acknowledgement or rejection is released.
pub(crate) struct Call {
    /// Remote account address in this held `accountSubscribe` request.
    pub(crate) pubkey: Pubkey,
    id: Value,
    reply: oneshot::Sender<Value>,
}

impl Call {
    /// Assigns the provider ID later used by notifications and unsubscribe requests.
    pub(crate) fn ack(self, subscription: u64) {
        self.reply.send(rpc_result(&self.id, subscription)).unwrap();
    }

    /// Rejects only this subscription without disconnecting the socket.
    pub(crate) fn reject(self) {
        self.reply
            .send(json!({"jsonrpc":RPC_VERSION, "id":self.id,
            "error":{"code":-32602,"message":"scripted rejection"}}))
            .unwrap();
    }
}

/// Holds acknowledgements and delivers ordered notifications on one controlled provider socket.
pub(crate) struct Server {
    pub(crate) endpoint: Url,
    calls: mpsc::UnboundedReceiver<Call>,
    unsubscribes: mpsc::UnboundedReceiver<u64>,
    output: mpsc::UnboundedSender<Message>,
    connected: mpsc::UnboundedReceiver<()>,
    tasks: Tasks,
}

impl Server {
    /// Accepts replacement sockets, auto-acknowledges releases, and holds subscription replies.
    pub(crate) async fn new() -> Self {
        let listener = TcpListener::bind(LOOPBACK).await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap()).parse().unwrap();
        let (tx, calls) = mpsc::unbounded_channel();
        let (released, unsubscribes) = mpsc::unbounded_channel();
        let (output, mut rx) = mpsc::unbounded_channel();
        let (ready, connected) = mpsc::unbounded_channel();
        let tasks = Tasks::default();
        tasks.spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let mut socket = accept_async(socket).await.unwrap();
                ready.send(()).unwrap();
                loop {
                    tokio::select! {
                        Some(message) = rx.recv() => {
                            let closing = matches!(message, Message::Close(_));
                            if socket.send(message).await.is_err() || closing { break; }
                        }
                        message = socket.next() => {
                            let text = match message {
                                Some(Ok(Message::Text(text))) => text,
                                Some(Ok(Message::Ping(data))) => {
                                    socket.send(Message::Pong(data)).await.unwrap();
                                    continue;
                                }
                                _ => break,
                            };
                            let message: Value = serde_json::from_str(&text).unwrap();
                            let batch = match message {
                                Value::Array(batch) => batch,
                                body => vec![body],
                            };
                            for body in batch {
                                if body["method"] == "accountUnsubscribe" {
                                    released.send(body["params"][0].as_u64().unwrap()).unwrap();
                                    let response = rpc_result(&body["id"], true);
                                    socket.send(Message::Text(response.to_string().into())).await.unwrap();
                                    continue;
                                }
                                let pubkey = body["params"][0].as_str().unwrap().parse().unwrap();
                                let id = body["id"].clone();
                                let (reply, response) = oneshot::channel();
                                tx.send(Call { pubkey, id, reply }).unwrap();
                                // Hold the batch at this request until the scenario acknowledges or rejects it.
                                let reply = response.await.unwrap();
                                socket.send(Message::Text(reply.to_string().into())).await.unwrap();
                            }
                        }
                    }
                }
            }
        });
        Self {
            endpoint,
            calls,
            unsubscribes,
            output,
            connected,
            tasks,
        }
    }

    /// Waits for the provider socket, not for account subscription coverage.
    pub(crate) async fn wait_for_connection(&mut self) {
        within(self.connected.recv()).await.expect("provider connection");
    }

    /// Receives the next held subscription request, preserving batch order.
    pub(crate) async fn next(&mut self) -> Call {
        within(self.calls.recv()).await.expect("subscription request")
    }

    /// Receives an observed unsubscribe ID independently of subscription scripts.
    pub(crate) async fn unsubscribed(&mut self) -> u64 {
        within(self.unsubscribes.recv()).await.expect("unsubscribe request")
    }

    /// Queues a confirmed notification; retired IDs deliberately exercise late delivery.
    pub(crate) fn notify(&self, subscription: u64, slot: u64, account: &Account) {
        let update = json!({"jsonrpc":RPC_VERSION, "method":"accountNotification", "params":{
            "subscription":subscription, "result":{"context":{"slot":slot},"value":account.rpc()}}});
        self.output.send(Message::Text(update.to_string().into())).unwrap();
    }

    /// Closes the current socket while keeping the listener available for reconnects.
    pub(crate) fn disconnect(&self) {
        self.output.send(Message::Close(None)).unwrap();
    }

    /// Cancels socket I/O and held acknowledgements, then joins the provider task.
    pub(crate) async fn close(self) {
        self.tasks.close().await;
    }
}
