use std::sync::Arc;

use magicblock_config::config::ApertureConfig;
use magicblock_core::link::{
    accounts::AccountUpdateRx,
    blocks::{BlockUpdateRx, LatestBlockInner},
    transactions::TransactionStatusRx,
    DispatchEndpoints,
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
    /// A receiver for block update events from the ledger.
    block_update_rx: BlockUpdateRx<LatestBlockInner>,
    /// An entry point for communicating with loaded geyser plugins
    geyser: Arc<GeyserPluginManager>,
}

/// Event-processing service prepared during RPC initialization and started only
/// after validator recovery is complete.
///
/// Deferring the block subscription keeps blockhashes broadcast during ledger
/// replay out of the RPC cache while preserving those broadcasts for the
/// execution runtime.
pub struct EventProcessors {
    config: ApertureConfig,
    state: SharedState,
    account_update_rx: AccountUpdateRx,
    transaction_status_rx: TransactionStatusRx,
    cancel: CancellationToken,
    geyser: Arc<GeyserPluginManager>,
}

impl EventProcessors {
    pub(crate) fn try_new(
        config: &ApertureConfig,
        state: SharedState,
        channels: &DispatchEndpoints,
        cancel: CancellationToken,
    ) -> ApertureResult<Self> {
        // SAFETY:
        // Geyser plugins use FFI. Operators must ensure each configured plugin
        // is compatible with this validator.
        let geyser =
            unsafe { GeyserPluginManager::new(&config.geyser_plugins) }?.into();
        Ok(Self {
            config: config.clone(),
            state,
            account_update_rx: channels.account_update.clone(),
            transaction_status_rx: channels.transaction_status.clone(),
            cancel,
            geyser,
        })
    }

    /// Starts workers and subscribes them to block updates.
    ///
    /// Call this after ledger replay. Broadcast receivers only observe updates
    /// sent after subscription, so replayed blockhashes cannot become valid RPC
    /// blockhashes.
    pub fn start(self) {
        let Self {
            config,
            state,
            account_update_rx,
            transaction_status_rx,
            cancel,
            geyser,
        } = self;
        for id in 0..config.event_processors {
            let processor = EventProcessor::new(
                &state,
                account_update_rx.clone(),
                transaction_status_rx.clone(),
                geyser.clone(),
            );
            tokio::spawn(processor.run(id, cancel.clone()));
        }
    }
}

impl EventProcessor {
    /// Creates a new `EventProcessor` instance by cloning handles to shared state and channels.
    fn new(
        state: &SharedState,
        account_update_rx: AccountUpdateRx,
        transaction_status_rx: TransactionStatusRx,
        geyser: Arc<GeyserPluginManager>,
    ) -> Self {
        let latest_block = state.ledger.latest_block().clone();
        // Subscribe to block updates immediately to ensure we don't miss any
        // notifications that might be sent before the `run()` method is polled.
        let block_update_rx = latest_block.subscribe();
        Self {
            subscriptions: state.subscriptions.clone(),
            transactions: state.transactions.clone(),
            blocks: state.blocks.clone(),
            account_update_rx,
            transaction_status_rx,
            block_update_rx,
            geyser,
        }
    }

    /// The main event processing loop for a single worker instance.
    #[instrument(skip(self, cancel), fields(processor_id = id))]
    async fn run(self, id: usize, cancel: CancellationToken) {
        info!("Event processor started");
        let mut block_update_rx = self.block_update_rx;
        loop {
            tokio::select! {
                biased;

                // Process a new block. We use `recv()` which returns `Ok(())` on
                // success or `Err(Lagged)` if we fell behind. In either case, we
                // want to update with the latest block. Only `Err(Closed)` should
                // stop us, but that's handled by the cancel token.
                Ok(latest) = block_update_rx.recv() => {
                    // Notify subscribers waiting on slot updates.
                    self.subscriptions.send_slot(latest.slot);
                    // Notify registered geyser plugins (if any) of the latest slot.
                    let _ = self.geyser.notify_slot(latest.slot).inspect_err(|e| {
                        warn!(error = ?e, "Geyser slot update failed");
                    });
                    // Notify listening geyser plugins
                    let _ = self.geyser.notify_block(&latest).inspect_err(|e| {
                        warn!(error = ?e, "Geyser block update failed");
                    });
                    // Update the global blocks cache with the latest block.
                    self.blocks.set_latest(&latest);
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
