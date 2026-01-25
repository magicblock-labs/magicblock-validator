use std::sync::Arc;

use magicblock_config::config::ApertureConfig;
use magicblock_core::link::{
    accounts::AccountUpdateRx, blocks::BlockUpdateRx,
    transactions::TransactionStatusRx, DispatchEndpoints,
};
use tokio_util::sync::CancellationToken;
use tracing::{info, instrument, warn};

use crate::{
    geyser::GeyserPluginManager,
    state::{
        blocks::BlocksCache,
        subscriptions::SubscriptionsDb,
        transactions::{SignatureResult, TransactionsCache},
        SharedState,
    },
    ApertureResult,
};

/// A worker that processes and dispatches validator events.
///
/// This processor listens for three main event types:
/// - Account Updates
/// - Transaction Status Updates
/// - New Block Productions
///
/// Its primary responsibilities are to forward these events to downstream subscribers
/// (e.g., WebSocket or Geyser clients) and to maintain the RPC service's shared
/// caches for transactions and blocks.
///
/// The design allows for multiple instances to be spawned concurrently, enabling
/// load balancing of event processing on a busy node.
pub(crate) struct EventProcessor {
    /// A handle to the global database of RPC subscriptions.
    subscriptions: SubscriptionsDb,
    /// A handle to the global cache of transaction statuses. This serves two purposes:
    /// 1. To provide a 75-second (~187 solana slots) window to prevent transaction replay.
    /// 2. To serve `getSignatureStatuses` RPC requests efficiently without querying the ledger.
    transactions: TransactionsCache,
    /// A handle to the global cache of recently produced blocks. This serves several purposes:
    /// 1. To verify that incoming transactions use a recent, valid blockhash.
    /// 2. To serve `isBlockhashValid` RPC requests efficiently.
    /// 3. To provide quick access to the latest blockhash and block height.
    blocks: Arc<BlocksCache>,
    /// A receiver for account update events, sourced from the `TransactionExecutor`.
    account_update_rx: AccountUpdateRx,
    /// A receiver for transaction status events, sourced from the `TransactionExecutor`.
    transaction_status_rx: TransactionStatusRx,
    /// A receiver for new block events.
    block_update_rx: BlockUpdateRx,
    /// An entry point for communicating with loaded geyser plugins
    geyser: Arc<GeyserPluginManager>,
}

impl EventProcessor {
    /// Creates a new `EventProcessor` instance by cloning handles to shared state and channels.
    fn new(
        channels: &DispatchEndpoints,
        state: &SharedState,
        geyser: Arc<GeyserPluginManager>,
    ) -> ApertureResult<Self> {
        Ok(Self {
            subscriptions: state.subscriptions.clone(),
            transactions: state.transactions.clone(),
            blocks: state.blocks.clone(),
            account_update_rx: channels.account_update.clone(),
            transaction_status_rx: channels.transaction_status.clone(),
            block_update_rx: channels.block_update.clone(),
            geyser,
        })
    }

    /// Spawns a specified number of `EventProcessor` workers.
    ///
    /// Each worker runs in its own Tokio task and will gracefully shut down when the
    /// provided `CancellationToken` is triggered.
    ///
    /// # Arguments
    /// * `state` - The shared global state of the RPC service.
    /// * `channels` - The endpoints for receiving validator events.
    /// * `instances` - The number of concurrent worker tasks to spawn.
    /// * `cancel` - The token used for graceful shutdown.
    pub(crate) fn start(
        config: &ApertureConfig,
        state: &SharedState,
        channels: &DispatchEndpoints,
        cancel: CancellationToken,
    ) -> ApertureResult<()> {
        // SAFETY:
        // Geyser plugin system works with the FFI, and is inherently unsafe,
        // the plugin must be 100% compatible with the validator to be used
        // without any memory violations, and it is the responsibility of the
        // node operator to ensure that loaded plugin is correct and safe to use.
        let geyser: Arc<_> =
            unsafe { GeyserPluginManager::new(&config.geyser_plugins) }?.into();
        for id in 0..config.event_processors {
            let geyser = geyser.clone();
            let processor = EventProcessor::new(channels, state, geyser)?;
            tokio::spawn(processor.run(id, cancel.clone()));
        }
        Ok(())
    }

    /// The main event processing loop for a single worker instance.
    #[instrument(skip(self, cancel), fields(processor_id = id))]
    async fn run(self, id: usize, cancel: CancellationToken) {
        info!("Event processor started");
        loop {
            tokio::select! {
                biased;

                // Process a new block.
                Ok(latest) = self.block_update_rx.recv_async() => {
                    // Notify subscribers waiting on slot updates.
                    self.subscriptions.send_slot(latest.meta.slot);
                    // Notify registered geyser plugins (if any) of the latest slot.
                    let _ = self.geyser.notify_slot(latest.meta.slot).inspect_err(|e| {
                        warn!(error = ?e, "Geyser slot update failed");
                    });
                    // Notify listening geyser plugins
                    let _ = self.geyser.notify_block(&latest).inspect_err(|e| {
                        warn!(error = ?e, "Geyser block update failed");
                    });
                    // Update the global blocks cache with the latest block.
                    self.blocks.set_latest(latest);
                }

                // Process a new account state update.
                Ok(state) = self.account_update_rx.recv_async() => {
                    // Notify subscribers for this specific account.
                    self.subscriptions.send_account_update(&state).await;
                    // Notify subscribers for the program that owns the account.
                    self.subscriptions.send_program_update(&state).await;
                    // Notify registered geyser plugins (if any) of the account.
                    let _ = self.geyser.notify_account(&state).inspect_err(|e| {
                        warn!(error = ?e, "Geyser account update failed");
                    });
                }

                // Process a new transaction status update.
                Ok(status) = self.transaction_status_rx.recv_async() => {
                    // Notify subscribers waiting on this specific transaction signature.
                    self.subscriptions.send_signature_update(&status).await;

                    // Notify subscribers interested in transaction logs.
                    self.subscriptions.send_logs_update(&status, status.slot);

                    // Notify listening geyser plugins
                    let _ = self.geyser.notify_transaction(&status).inspect_err(|e| {
                        warn!(error = ?e, "Geyser transaction update failed");
                    });

                    // Update the global transaction cache.
                    let result = SignatureResult {
                        slot: status.slot,
                        result: status.meta.status
                    };
                    self.transactions.push(*status.txn.signature(), Some(result));
                }
                // Listen for the cancellation signal to gracefully shut down.
                _ = cancel.cancelled() => {
                    break;
                }
            }
        }
        info!("Event processor terminated");
    }
}
