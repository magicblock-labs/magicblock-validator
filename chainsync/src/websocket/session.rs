use std::time::Duration;

use super::{
    ConnectionId, Error, Event, Result,
    transport::{self, MAX_MESSAGE, Reader, Writer},
};
use crate::AccountSubscription;
use crate::rpc::{AccountConfig, ContextValue, Error as RpcError, Request, WireAccount};
use ahash::AHashMap;
use fastwebsockets::{Frame, OpCode, Payload};
use futures::{
    StreamExt,
    future::{self, AbortHandle, Abortable},
    stream::{self, FuturesUnordered},
};
use json::{JsonValueTrait, LazyValue};
use serde::{Deserialize, Serialize};
use solana_account::AccountBuilder;
use solana_pubkey::Pubkey;
use tokio::{
    sync::mpsc::{Sender, UnboundedReceiver, UnboundedSender},
    time::{self, Instant, MissedTickBehavior, Sleep},
};
use url::Url;

const ACCOUNT_SUBSCRIBE: &str = "accountSubscribe";
const ACCOUNT_UNSUBSCRIBE: &str = "accountUnsubscribe";
const ACCOUNT_NOTIFICATION: &str = "accountNotification";

/// Account operations assigned to one connection attempt.
pub(super) enum Command {
    /// Subscription whose pool capacity is already reserved.
    Subscribe(AccountSubscription),
    /// Release of an acknowledged provider subscription.
    Unsubscribe {
        pubkey: Pubkey,
        /// Acknowledged provider subscription ID.
        remote: u64,
    },
}

/// Socket outcomes consumed by the pool registry.
pub(super) enum Notice {
    /// Connection is accepting commands.
    Connected(ConnectionId),
    /// Provider result for a subscribe or unsubscribe request.
    Acknowledged {
        connection: ConnectionId,
        pubkey: Pubkey,
        /// Provider subscription ID on subscribe, none on unsubscribe.
        result: Result<Option<u64>>,
    },
    /// All subscriptions on this attempt were lost.
    Dropped {
        connection: ConnectionId,
        /// Connection failure logged by the pool before reporting lost subscriptions.
        error: Error,
    },
}

/// Connection and RPC acknowledgement budget.
const TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const COMMAND_CAP: usize = 256;
/// Ping cadence; a missing pong invalidates the connection.
const HEARTBEAT: Duration = Duration::from_secs(15);

/// Sent request awaiting its remote acknowledgement.
struct Pending {
    /// Operation used to interpret the response.
    command: Command,
    /// Cancelled after response correlation.
    timer: AbortHandle,
}

/// Owns protocol state and I/O for one connection attempt.
pub(super) struct Session {
    /// Exclusive outbound half for RPC and control replies.
    writer: Writer,
    /// Monotonic request ID within this attempt.
    sequence: u64,
    /// Request IDs correlated with unacknowledged commands.
    pending: AHashMap<u64, Pending>,
    /// Acknowledgement deadlines for pending commands.
    timers: FuturesUnordered<Abortable<Sleep>>,
    /// Maps provider subscription IDs to remote addresses and the Engine accounts to update.
    active: AHashMap<u64, AccountSubscription>,
    id: ConnectionId,
    /// Registry-only lifecycle channel.
    notices: UnboundedSender<Notice>,
    /// Reused across serialization and transport masking.
    output: Vec<u8>,
    events: Sender<Event>,
}

impl Session {
    /// Runs one attempt and reports loss only after its I/O is closed.
    pub(super) async fn start(
        id: ConnectionId,
        url: Url,
        mut commands: UnboundedReceiver<Command>,
        events: Sender<Event>,
        notices: UnboundedSender<Notice>,
        delay: Duration,
    ) {
        let result = async {
            time::sleep_until(Instant::now() + delay).await;
            let (reader, writer) = time::timeout(TIMEOUT, transport::connect(&url))
                .await
                .map_err(|_| Error::Timeout("connect"))??;
            let mut session = Self {
                writer,
                sequence: 0,
                id,
                notices: notices.clone(),
                pending: AHashMap::new(),
                timers: FuturesUnordered::new(),
                active: AHashMap::new(),
                output: Vec::new(),
                events,
            };
            session.notify(Notice::Connected(id))?;
            session.run(reader, &mut commands).await
        }
        .await;
        // Close the command receiver and drop the socket before reporting subscription loss.
        commands.close();
        if let Err(error) = result {
            let _ = notices.send(Notice::Dropped { connection: id, error });
        }
    }

    /// Multiplexes commands, reads, deadlines, and heartbeats without losing frame state.
    async fn run(
        &mut self,
        reader: Reader,
        commands: &mut UnboundedReceiver<Command>,
    ) -> Result<()> {
        let mut heartbeat = time::interval_at(Instant::now() + HEARTBEAT, HEARTBEAT);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut awaiting_pong = false;
        let mut batch = Vec::with_capacity(COMMAND_CAP);
        // Keep the in-flight read pinned across select branches so framing survives cancellation.
        let frames = stream::unfold(reader, |mut reader| async {
            // Automatic replies are disabled; all writes stay on the session's writer.
            let mut send =
                |_| async { Err::<(), _>(Error::Protocol("unexpected automatic reply")) };
            let frame = reader.read_frame(&mut send).await;
            Some((frame, reader))
        });
        tokio::pin!(frames);
        // Writes and event delivery are untimed. While either waits, this socket cannot
        // read or check deadlines, but their budgets continue to elapse.
        loop {
            tokio::select! {
                biased;
                count = commands.recv_many(&mut batch, COMMAND_CAP) => {
                    if count == 0 {
                        return Ok(());
                    }
                    if count > 1 {
                        self.output.push(b'[');
                    }
                    for (index, command) in batch.drain(..).enumerate() {
                        if index > 0 {
                            self.output.push(b',');
                        }
                        match command {
                            Command::Subscribe(account) => {
                                let params = (account.pubkey.to_string(), AccountConfig::new(None));
                                self.request(Command::Subscribe(account), params)?;
                            }
                            Command::Unsubscribe { pubkey, remote } => {
                                self.request(Command::Unsubscribe { pubkey, remote }, [remote])?;
                            }
                        }
                    }
                    if count > 1 {
                        self.output.push(b']');
                    }
                    // Reuse the nonempty batch buffer for transport masking and serialization.
                    let payload = self.output.as_mut_slice().into();
                    self.writer.write_frame(Frame::text(payload)).await?;
                    self.output.clear();
                }
                // Socket reads and deadlines must progress even under command pressure.
                Some(frame) = frames.next() => {
                    let frame = frame?;
                    match frame.opcode {
                        OpCode::Text | OpCode::Binary => {
                            // The collector has already allocated the complete message.
                            if frame.payload.len() > MAX_MESSAGE {
                                return Err(Error::Protocol("message exceeds size limit"));
                            }
                            self.message(&frame.payload).await?;
                        }
                        OpCode::Continuation => {
                            return Err(Error::Protocol("unexpected continuation"));
                        }
                        OpCode::Ping | OpCode::Pong if frame.payload.len() > 125 => {
                            return Err(Error::Protocol("oversized control frame"));
                        }
                        OpCode::Ping => {
                            self.writer.write_frame(Frame::pong(frame.payload)).await?;
                        }
                        OpCode::Pong => awaiting_pong = false,
                        OpCode::Close => {
                            let _ = self.writer.write_frame(Frame::close(1000, b"")).await;
                            return Err(Error::Disconnected);
                        }
                    }
                }
                _ = self.expired() => {
                    return Err(Error::Timeout("RPC acknowledgement"));
                }
                _ = heartbeat.tick() => {
                    if awaiting_pong { return Err(Error::Timeout("heartbeat")); }
                    let frame = Frame::new(true, OpCode::Ping, None, Payload::Borrowed(b""));
                    self.writer.write_frame(frame).await?;
                    awaiting_pong = true;
                }
            }
        }
    }

    /// Waits for an uncancelled acknowledgement deadline.
    async fn expired(&mut self) {
        while let Some(result) = self.timers.next().await {
            if result.is_ok() {
                return;
            }
        }
        future::pending().await
    }

    /// Correlates and serializes an operation with its deadline.
    fn request(&mut self, command: Command, params: impl Serialize) -> Result<()> {
        let (timer, registration) = AbortHandle::new_pair();
        self.timers.push(Abortable::new(
            time::sleep_until(Instant::now() + TIMEOUT),
            registration,
        ));
        self.sequence += 1;
        let id = self.sequence;
        let method = match &command {
            Command::Subscribe(_) => ACCOUNT_SUBSCRIBE,
            Command::Unsubscribe { .. } => ACCOUNT_UNSUBSCRIBE,
        };
        self.pending.insert(id, Pending { command, timer });
        let request = Request::new(id, method, params);
        json::to_writer(&mut self.output, &request)?;
        Ok(())
    }

    /// Accepts single or batched RPC envelopes, rejecting malformed batch tails.
    async fn message(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'[') {
            // The lazy iterator stops at `]`; validate the whole message to reject trailing data.
            let batch: LazyValue<'_> = json::from_slice(bytes)?;
            let mut messages = json::to_array_iter(batch.as_raw_str()).peekable();
            if messages.peek().is_none() {
                return Err(Error::Protocol("empty RPC batch"));
            }
            for message in messages {
                self.envelope(message?.as_raw_str().as_bytes()).await?;
            }
        } else {
            self.envelope(bytes).await?;
        }
        Ok(())
    }

    /// Validates routing before decoding a response or account update.
    async fn envelope(&mut self, bytes: &[u8]) -> Result<()> {
        let message: Envelope<'_> = json::from_slice(bytes)?;
        if let Some(id) = message.id {
            if message.result.is_some() == message.error.is_some() {
                return Err(Error::Protocol("invalid RPC response envelope"));
            }
            let pending = self.pending.remove(&id).ok_or(Error::Protocol("unknown request ID"))?;
            pending.timer.abort();
            return self.response(pending, message.result, message.error);
        }
        if message.method != Some(ACCOUNT_NOTIFICATION) {
            return Err(Error::Protocol("invalid notification method"));
        }
        let notification = message.params.ok_or(Error::Protocol("missing notification params"))?;
        // Logical removal can precede delivery of notifications buffered by the provider.
        // An inactive ID has no authorized local target; ignore it without dropping other coverage.
        let Some(&sub) = self.active.get(&notification.subscription) else {
            return Ok(());
        };
        let account: ContextValue<Option<WireAccount<'_>>> =
            json::from_str(notification.result.as_raw_str())?;
        let slot = account.context.slot;
        let account = match account.value {
            Some(value) => value.decode(slot)?,
            None => AccountBuilder::default(),
        };
        self.events
            .send(Event::Update { sub, account })
            .await
            .map_err(|_| Error::Closed)
    }

    /// Updates remote-ID routing before reporting an operation outcome.
    fn response(
        &mut self,
        pending: Pending,
        result: Option<LazyValue<'_>>,
        error: Option<LazyValue<'_>>,
    ) -> Result<()> {
        if let Some(error) = error {
            let error: RpcError = json::from_str(error.as_raw_str())?;
            // Rejected unsubscribe leaves remote capacity ambiguous.
            let Command::Subscribe(account) = pending.command else {
                return Err(Error::Rpc(error));
            };
            return self.notify(Notice::Acknowledged {
                connection: self.id,
                pubkey: account.pubkey,
                result: Err(Error::Rpc(error)),
            });
        }
        let result = result.ok_or(Error::Protocol("missing RPC result"))?;
        let (pubkey, remote) = match pending.command {
            Command::Unsubscribe { pubkey, remote } => {
                if result.as_bool() != Some(true) {
                    return Err(Error::Protocol("unsubscribe was not acknowledged"));
                }
                self.active.remove(&remote);
                (pubkey, None)
            }
            Command::Subscribe(account) => {
                let remote =
                    result.as_u64().ok_or(Error::Protocol("invalid remote subscription ID"))?;
                // Route notifications before waking the pool's subscribe waiter.
                if self.active.insert(remote, account).is_some() {
                    return Err(Error::Protocol("duplicate remote subscription ID"));
                }
                (account.pubkey, Some(remote))
            }
        };
        self.notify(Notice::Acknowledged {
            connection: self.id,
            pubkey,
            result: Ok(remote),
        })
    }

    /// Reports connection and RPC outcomes through the pool's unbounded channel.
    fn notify(&self, notice: Notice) -> Result<()> {
        self.notices.send(notice).map_err(|_| Error::Closed)
    }
}

/// Borrowed RPC envelope; decode its payload only after checking the request or subscription ID.
#[derive(Deserialize)]
struct Envelope<'a> {
    /// Notification method when no request ID is present.
    method: Option<&'a str>,
    /// Presence distinguishes responses from notifications.
    id: Option<u64>,
    /// Success payload interpreted by the pending operation.
    #[serde(borrow)]
    result: Option<LazyValue<'a>>,
    /// Provider rejection decoded after request correlation.
    #[serde(borrow)]
    error: Option<LazyValue<'a>>,
    /// Notification routing data, absent from responses.
    #[serde(borrow)]
    params: Option<Notification<'a>>,
}

/// Account update tied to a provider subscription ID.
#[derive(Deserialize)]
struct Notification<'a> {
    /// Provider-issued ID valid only on this connection.
    subscription: u64,
    /// Account state decoded after subscription routing.
    #[serde(borrow)]
    result: LazyValue<'a>,
}
