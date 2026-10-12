use std::time::Duration;

use ahash::AHashMap;
use nucleus::shutdown::{Service, ShutdownManager, ShutdownReason};
use tokio::{
    sync::{
        mpsc::{self, Receiver, Sender, UnboundedReceiver, UnboundedSender},
        oneshot,
    },
    task::JoinHandle,
};

use super::{
    Config, ConnectionId, Error, Event, Result,
    session::{COMMAND_CAP, Command, Notice, Session},
};
use crate::AccountSubscription;
use crate::metrics::{self, Transport};
use solana_pubkey::Pubkey;
use tracing::{info, warn};

/// Subscription handle for a shutdown-managed WebSocket pool.
pub(crate) struct Pool {
    /// Bounded queue for subscription and unsubscribe requests.
    commands: Sender<SubscriptionRequest>,
}

/// Reports whether a reference changed; removal callers discard the boolean.
type Reply = oneshot::Sender<Result<bool>>;

/// Subscription request processed by the pool registry.
enum SubscriptionRequest {
    /// Adds a reference, or reuses an active resident mirror's reference when requested.
    Subscribe {
        /// Remote address and local destination that must match an existing reference.
        account: AccountSubscription,
        /// Resident targets can keep their existing mirror reference without acquiring another.
        reuse: bool,
        /// Reports whether this request acquired a reference that needs failure cleanup.
        reply: Reply,
    },
    /// Releases one reference for a remote address, waiting for acknowledgement if it was the last.
    Unsubscribe(Pubkey, Reply),
    /// Removes all active remote subscriptions feeding a local address, ignoring reference counts.
    UnsubscribeAccount(Pubkey),
}

/// Per-account state that occupies socket capacity.
enum Subscription {
    /// Subscribe request awaiting a server acknowledgement.
    Pending {
        /// Completes only after provider acknowledgement or a terminal failure.
        reply: Reply,
        /// Remote address and local account reported when the server acknowledges.
        account: AccountSubscription,
        /// Unsubscribe caller waiting for a pending subscribe to finish before cancellation.
        cancel: Option<Reply>,
    },
    /// Acknowledged subscription shared by identical acquisition requests.
    Active {
        /// Provider subscription ID used to unsubscribe.
        remote: u64,
        /// Remote address and local account; requests can share only if both match.
        account: AccountSubscription,
        /// Number of acquisition requests sharing this subscription; the last release unsubscribes.
        refs: usize,
    },
    /// Unsubscribe awaiting acknowledgement or socket loss.
    Unsubscribing(Reply),
}

/// Pool entry whose capacity remains allocated across reconnects.
struct Socket {
    /// Current attempt identity; reconnect advances its generation.
    id: ConnectionId,
    commands: UnboundedSender<Command>,
    /// Aborted when this entry is replaced or dropped.
    task: JoinHandle<()>,
    /// Requested account subscriptions and pending replies.
    accounts: AHashMap<Pubkey, Subscription>,
    ready: bool,
    /// Reconnect delay reset after observed readiness.
    backoff: Duration,
}

impl Socket {
    fn healthy(&self) -> bool {
        self.ready && !self.commands.is_closed()
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Pool {
    /// Starts connecting on the current Tokio runtime without waiting for
    /// readiness. The pool and its socket tasks stop on coordinated shutdown.
    /// Connection failures arrive as [`Event::Dropped`].
    pub(crate) fn new(config: Config, manager: &mut ShutdownManager) -> (Self, Receiver<Event>) {
        let (commands, requests) = mpsc::channel(COMMAND_CAP);
        let (events, receiver) = mpsc::channel(EVENT_CAP);
        let (notices, incoming) = mpsc::unbounded_channel();
        let mut registry = Registry {
            config,
            sockets: Vec::new(),
            routes: AHashMap::new(),
            events: events.clone(),
            notices,
            occupied: 0,
            capacity: 0,
            cursor: 0,
        };
        let mut shutdown = manager.handle(Service::ChainSyncWebSocket);
        tokio::spawn(async move {
            for provider in 0..registry.config.0.len() {
                registry.open(provider);
            }
            tokio::select! {
                _ = shutdown.signalled() => {},
                _ = events.closed() => {},
                _ = registry.run(requests, incoming) => {},
            }
            for socket in &mut registry.sockets {
                socket.task.abort();
                let _ = (&mut socket.task).await;
            }
            drop(registry);
            shutdown.terminate(ShutdownReason::Signalled);
        });
        (Self { commands }, receiver)
    }

    /// Retains an acknowledged subscription; identical active requests share one provider ID.
    /// `reuse` borrows an existing mirror reference instead of incrementing its count.
    /// Returns whether a new reference was acquired, for failure cleanup.
    /// Acknowledgement does not include an initial account image.
    /// Updates retain the local account to update, even if queued before unsubscribe completes.
    ///
    /// Capacity failures return immediately. Do not cancel: admitted work may
    /// complete after the caller stops waiting.
    pub(crate) async fn subscribe(
        &self,
        account: AccountSubscription,
        reuse: bool,
    ) -> Result<bool> {
        self.request(
            |account, reply| SubscriptionRequest::Subscribe { account, reuse, reply },
            account,
        )
        .await
    }

    /// Releases one reference for the remote address `pubkey`; the last release unsubscribes remotely.
    /// For Loader V3 ELF updates, pass the ProgramData address, not the local program address.
    /// Do not overlap ordinary subscribe and unsubscribe operations for the same remote address.
    ///
    /// Already-lost subscriptions are a no-op; buffered updates may still arrive.
    /// Connection loss returns [`Error::Disconnected`]; the registry logs its cause.
    pub(crate) async fn unsubscribe(&self, pubkey: Pubkey) -> Result<()> {
        self.request(SubscriptionRequest::Unsubscribe, pubkey).await.map(|_| ())
    }

    /// Queues removal of all active remote subscriptions feeding `local_pubkey`,
    /// regardless of reference count.
    /// For a Loader V3 program, this removes its ProgramData subscription too.
    /// Does not wait for provider acknowledgement; caller holds the local account lease until queued.
    pub(crate) async fn unsubscribe_account(&self, local_pubkey: Pubkey) -> Result<()> {
        self.commands
            .send(SubscriptionRequest::UnsubscribeAccount(local_pubkey))
            .await
            .map_err(|_| Error::Closed)
    }

    /// Queues a request and waits for the registry's reply.
    async fn request<T>(
        &self,
        make: impl FnOnce(T, Reply) -> SubscriptionRequest,
        value: T,
    ) -> Result<bool> {
        let (reply, result) = oneshot::channel();
        self.commands.send(make(value, reply)).await.map_err(|_| Error::Closed)?;
        result.await.map_err(|_| Error::Closed)?
    }
}

/// Owns subscription routing and capacity accounting for the pool.
struct Registry {
    /// Provider limits and reconnect policy.
    config: Config,
    /// Entries retain stable indices across reconnects.
    sockets: Vec<Socket>,
    /// Maps each remote account address to the socket holding its subscription state.
    routes: AHashMap<Pubkey, usize>,
    /// Account updates, subscription acknowledgements, removals, and connection losses.
    events: Sender<Event>,
    /// Socket outcomes arrive independently of public updates.
    notices: UnboundedSender<Notice>,
    /// Capacity occupied by requested account subscription states.
    occupied: usize,
    /// Capacity allocated to all entries, including connecting sockets.
    capacity: usize,
    cursor: usize,
}

impl Registry {
    /// Serializes subscription requests and socket outcomes before replying or changing routes.
    async fn run(
        &mut self,
        mut requests: Receiver<SubscriptionRequest>,
        mut notices: UnboundedReceiver<Notice>,
    ) {
        loop {
            tokio::select! {
                request = requests.recv() => {
                    let Some(request) = request else { return };
                    match request {
                        SubscriptionRequest::Subscribe { account, reuse, reply } => {
                            self.subscribe(account, reuse, reply);
                        }
                        SubscriptionRequest::Unsubscribe(pubkey, reply) => {
                            self.unsubscribe(pubkey, reply).await;
                        }
                        SubscriptionRequest::UnsubscribeAccount(local_pubkey) => {
                            self.unsubscribe_account(local_pubkey).await;
                        }
                    }
                }
                Some(notice) = notices.recv() => self.notice(notice).await,
            }
        }
    }

    /// Removes matching direct and ProgramData subscriptions for a local address.
    /// Unlike `unsubscribe`, releases all references rather than one remote-address reference.
    async fn unsubscribe_account(&mut self, local_pubkey: Pubkey) {
        for sub in AccountSubscription::for_account(local_pubkey) {
            let pubkey = sub.pubkey;
            let Some(&index) = self.routes.get(&pubkey) else { continue };
            let Some(Subscription::Active { account, refs, .. }) =
                self.sockets[index].accounts.get_mut(&pubkey)
            else {
                continue;
            };
            if *account != sub {
                continue;
            }
            // Force unsubscribe even when several acquisition requests share this subscription.
            *refs = 1;
            let (reply, _) = oneshot::channel();
            self.unsubscribe(pubkey, reply).await;
        }
    }

    /// Reserves capacity before waiting for remote acknowledgement.
    fn subscribe(&mut self, account: AccountSubscription, reuse: bool, reply: Reply) {
        let pubkey = account.pubkey;
        if let Some(&index) = self.routes.get(&pubkey) {
            // Overlapping acquisition waves share an acknowledged subscription.
            let result = match self.sockets[index].accounts.get_mut(&pubkey) {
                Some(Subscription::Active { account: current, refs, .. })
                    if *current == account =>
                {
                    if !reuse {
                        *refs += 1;
                    }
                    Ok(!reuse)
                }
                _ => Err(Error::Unavailable),
            };
            let _ = reply.send(result);
            return;
        }
        let index = match self.admit() {
            Ok(index) => index,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let socket = &mut self.sockets[index];
        if socket.commands.send(Command::Subscribe(account)).is_err() {
            let _ = reply.send(Err(Error::Unavailable));
            return;
        }
        socket.accounts.insert(
            pubkey,
            Subscription::Pending { reply, account, cancel: None },
        );
        self.routes.insert(pubkey, index);
        self.cursor = (index + 1) % self.sockets.len();
        let needed_growth = self.should_grow();
        self.occupied += 1;
        if !needed_growth && self.should_grow() {
            self.grow();
        }
    }

    /// Keeps capacity occupied until acknowledgement or socket loss.
    async fn unsubscribe(&mut self, pubkey: Pubkey, reply: Reply) {
        let Some(&index) = self.routes.get(&pubkey) else {
            // Connection loss may have removed this subscription before its release.
            let _ = reply.send(Ok(false));
            return;
        };
        if matches!(
            self.sockets[index].accounts.get(&pubkey),
            Some(Subscription::Active { refs: 1, .. })
        ) {
            // Tell the worker to reject buffered updates before sending provider unsubscribe.
            // Shared subscriptions reach this point only when their last reference is released.
            let _ = self.events.send(Event::Removed(pubkey)).await;
        }
        let socket = &mut self.sockets[index];
        let Some(state) = socket.accounts.get_mut(&pubkey) else { return };
        match state {
            Subscription::Active { refs, .. } if *refs > 1 => {
                *refs -= 1;
                let _ = reply.send(Ok(true));
            }
            Subscription::Active { remote, .. } => {
                let remote = *remote;
                *state = Subscription::Unsubscribing(reply);
                // If I/O has just stopped, its queued loss notice completes this waiter.
                let _ = socket.commands.send(Command::Unsubscribe { pubkey, remote });
            }
            Subscription::Pending { cancel, .. } => *cancel = Some(reply),
            Subscription::Unsubscribing(_) => {
                let _ = reply.send(Ok(false));
            }
        }
    }

    /// Finds ready capacity without queuing behind connection attempts.
    fn admit(&mut self) -> Result<usize> {
        let len = self.sockets.len();
        for index in (self.cursor..len).chain(0..self.cursor) {
            let socket = &self.sockets[index];
            if !socket.healthy() {
                continue;
            }
            let limit = self.config.0[socket.id.provider].subs_per_connection;
            if socket.accounts.len() < limit {
                return Ok(index);
            }
        }
        self.grow();
        let limit: usize = self.config.0.iter().map(|provider| provider.max_connections).sum();
        let full = self.occupied == self.capacity && self.sockets.len() == limit;
        Err(if full { Error::Capacity } else { Error::Unavailable })
    }

    /// Applies socket outcomes before replying to requests or reporting lost coverage.
    async fn notice(&mut self, notice: Notice) {
        match notice {
            Notice::Connected(connection) => {
                if connection.generation > 0 {
                    let provider = self.config.0[connection.provider].url.host_str();
                    info!(provider, "WS reconnected");
                }
                let socket = &mut self.sockets[connection.index];
                socket.ready = true;
                socket.backoff = Duration::ZERO;
            }
            Notice::Acknowledged { connection, pubkey, result } => {
                // Acknowledgements do not trigger pool growth.
                return self.acknowledge(connection, pubkey, result).await;
            }
            Notice::Dropped { connection, error } => {
                let provider = self.config.0[connection.provider].url.host_str();
                metrics::transport(Transport::WebSocket);
                let socket = &mut self.sockets[connection.index];
                self.occupied -= socket.accounts.len();
                let delay =
                    (socket.backoff * 2).clamp(Duration::from_secs(1), Duration::from_secs(30));
                let mut pubkeys = Vec::new();
                for (pubkey, entry) in socket.accounts.drain() {
                    self.routes.remove(&pubkey);
                    let report = match entry {
                        Subscription::Pending { reply, cancel, .. } => {
                            // A cancelled sub must not trigger account eviction on socket loss.
                            let report = cancel.is_none();
                            let _ = reply.send(Err(Error::Disconnected));
                            if let Some(reply) = cancel {
                                let _ = reply.send(Err(Error::Disconnected));
                            }
                            report
                        }
                        Subscription::Unsubscribing(reply) => {
                            let _ = reply.send(Err(Error::Disconnected));
                            false
                        }
                        Subscription::Active { .. } => true,
                    };
                    if report {
                        pubkeys.push(pubkey);
                    }
                }
                // The failed task has finished publishing updates. Publish loss before replacing
                // it or accepting new account subscriptions, preserving this connection's event order.
                warn!(provider, lost = pubkeys.len(), %error, "WS connection failed");
                let _ = self.events.send(Event::Dropped { pubkeys }).await;
                let id = ConnectionId {
                    generation: connection.generation + 1,
                    ..connection
                };
                self.sockets[connection.index] = self.spawn(id, delay);
            }
        }
        if self.should_grow() {
            self.grow();
        }
    }

    /// Updates subscription state and coverage before replying to the request.
    async fn acknowledge(
        &mut self,
        connection: ConnectionId,
        pubkey: Pubkey,
        result: Result<Option<u64>>,
    ) {
        let socket = &mut self.sockets[connection.index];
        let Some(state) = socket.accounts.remove(&pubkey) else { return };
        let (reply, cancel, result) = match (state, result) {
            (Subscription::Pending { reply, account, cancel }, Ok(Some(remote))) => {
                if let Some(cancel) = cancel {
                    // The caller already requested unsubscribe; do not report this as active.
                    socket.accounts.insert(pubkey, Subscription::Unsubscribing(cancel));
                    let _ = socket.commands.send(Command::Unsubscribe { pubkey, remote });
                } else {
                    socket
                        .accounts
                        .insert(pubkey, Subscription::Active { remote, account, refs: 1 });
                    let _ = self.events.send(Event::Acknowledged(account)).await;
                }
                let _ = reply.send(Ok(true));
                return;
            }
            (Subscription::Pending { reply, cancel, .. }, result) => (reply, cancel, result),
            (Subscription::Unsubscribing(reply), result @ (Ok(None) | Err(_))) => {
                (reply, None, result)
            }
            (state, _) => {
                socket.accounts.insert(pubkey, state);
                return;
            }
        };
        self.routes.remove(&pubkey);
        self.occupied -= 1;
        let _ = reply.send(result.map(|_| true));
        if let Some(cancel) = cancel {
            let _ = cancel.send(Ok(true));
        }
    }

    /// Starts a connection ready to accept requested account subscriptions.
    fn spawn(&self, id: ConnectionId, backoff: Duration) -> Socket {
        let (commands, receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(Session::start(
            id,
            self.config.0[id.provider].url.clone(),
            receiver,
            self.events.clone(),
            self.notices.clone(),
            backoff,
        ));
        Socket {
            id,
            commands,
            task,
            accounts: AHashMap::new(),
            ready: false,
            backoff,
        }
    }

    /// Requests more sockets at 75% of allocated pool-wide capacity.
    fn should_grow(&self) -> bool {
        self.occupied * 4 >= self.capacity * 3
    }

    /// Tries every provider so one full or disconnected provider does not
    /// prevent healthy providers from adding capacity.
    fn grow(&mut self) {
        for provider in 0..self.config.0.len() {
            self.grow_provider(provider);
        }
    }

    /// Adds up to one new socket per healthy socket within provider limits.
    fn grow_provider(&mut self, provider: usize) {
        let mut count = 0;
        let mut healthy = 0usize;
        for socket in self.sockets.iter().filter(|s| s.id.provider == provider) {
            count += 1;
            // Only fresh attempts block another growth batch. Reconnects still
            // consume the hard limit, but cannot hold healthy capacity back.
            if !socket.ready && socket.id.generation == 0 {
                return;
            }
            if socket.healthy() {
                healthy += 1;
            }
        }
        let config = &self.config.0[provider];
        // A full provider or one without healthy sockets naturally opens an empty batch.
        let batch = healthy.min(config.max_connections - count);
        for _ in 0..batch {
            self.open(provider);
        }
    }

    /// Allocates a pool entry and starts its first connection attempt.
    fn open(&mut self, provider: usize) {
        let id = ConnectionId {
            provider,
            index: self.sockets.len(),
            generation: 0,
        };
        let socket = self.spawn(id, Duration::ZERO);
        self.sockets.push(socket);
        self.capacity += self.config.0[provider].subs_per_connection;
    }
}

/// Maximum public events awaiting consumption across the pool.
const EVENT_CAP: usize = 8192;
