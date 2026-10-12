use engine::Engine;
use nucleus::shutdown::{Service, ShutdownManager, ShutdownReason};
use solana_pubkey::Pubkey;
use tokio::sync::mpsc;

use super::{Event, Result, StreamConfig, session::Session};
use crate::AccountSubscription;

/// Account-subscription changes processed by one gRPC stream.
pub(crate) enum Command {
    /// Tracks a WebSocket subscription; new gRPC filter entries wait for the duplication delay.
    Track {
        sub: AccountSubscription,
        /// Generation assigned by the coverage registry to reject stale confirmations.
        generation: u64,
    },
    /// Stops forwarding this account's updates when processed; filter removal waits for a rebuild.
    Remove(Pubkey),
}

/// Control handle for one shutdown-managed Yellowstone stream.
pub(crate) struct Client {
    commands: mpsc::UnboundedSender<Command>,
    hostname: Option<Box<str>>,
}

impl Client {
    /// Starts one provider session and ties its lifetime and failures to coordinated shutdown.
    pub(crate) fn new(
        id: usize,
        config: StreamConfig,
        engine: Engine,
        events: mpsc::Sender<Event>,
        manager: &mut ShutdownManager,
    ) -> Result<Self> {
        let (commands, requests) = mpsc::unbounded_channel();
        let hostname = config.endpoint.host_str().map(Box::from);
        let session = Session::new(id, config, engine, events)?;
        let mut shutdown = manager.handle(Service::ChainSyncGrpc(id));
        tokio::spawn(async move {
            let reason = tokio::select! {
                biased;
                _ = shutdown.signalled() => ShutdownReason::Signalled,
                Err(error) = session.run(requests) => {
                    ShutdownReason::Error(Box::new(error))
                },
            };
            shutdown.terminate(reason);
        });
        Ok(Self { commands, hostname })
    }

    pub(crate) fn hostname(&self) -> Option<&str> {
        self.hostname.as_deref()
    }

    /// Queues a subscription change without waiting for the stream to process or send it.
    pub(crate) fn command(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}
