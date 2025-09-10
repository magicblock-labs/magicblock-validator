use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};

use parking_lot::RwLock;
use solana_account::ReadableAccount;
use solana_pubkey::Pubkey;
use solana_signature::Signature;

use crate::{
    encoder::{
        AccountEncoder, Encoder, ProgramAccountEncoder, SlotEncoder,
        TransactionLogsEncoder, TransactionResultEncoder,
    },
    server::websocket::{
        connection::ConnectionID,
        dispatch::{ConnectionTx, WsConnectionChannel},
    },
};
use magicblock_core::{
    link::{
        accounts::AccountWithSlot,
        transactions::{TransactionResult, TransactionStatus},
    },
    Slot,
};

/// Manages subscriptions to changes in specific account. Maps a `Pubkey` to its subscribers.
pub(crate) type AccountSubscriptionsDb =
    Arc<scc::HashMap<Pubkey, UpdateSubscribers<AccountEncoder>>>;
/// Manages subscriptions to accounts owned by a specific program. Maps a program `Pubkey` to its subscribers.
pub(crate) type ProgramSubscriptionsDb =
    Arc<scc::HashMap<Pubkey, UpdateSubscribers<ProgramAccountEncoder>>>;
/// Manages one-shot subscriptions for transaction signature statuses. Maps a `Signature` to its subscriber.
pub(crate) type SignatureSubscriptionsDb =
    Arc<scc::HashMap<Signature, UpdateSubscriber<TransactionResultEncoder>>>;
/// Manages subscriptions to all transaction logs.
pub(crate) type LogsSubscriptionsDb =
    Arc<RwLock<UpdateSubscribers<TransactionLogsEncoder>>>;
/// Manages subscriptions to slot updates.
pub(crate) type SlotSubscriptionsDb =
    Arc<RwLock<UpdateSubscriber<SlotEncoder>>>;

/// A unique identifier for a single subscription, returned to the client.
pub(crate) type SubscriptionID = u64;

/// A global atomic counter for generating unique subscription IDs.
static SUBID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The central database for managing all WebSocket pub/sub subscriptions.
///
/// This struct aggregates different subscription types (accounts, programs, etc.)
/// into a single, cloneable unit that can be shared across the application.
#[derive(Clone)]
pub(crate) struct SubscriptionsDb {
    /// Subscriptions for individual account changes.
    pub(crate) accounts: AccountSubscriptionsDb,
    /// Subscriptions for accounts owned by a specific program.
    pub(crate) programs: ProgramSubscriptionsDb,
    /// One-shot subscriptions for transaction signature statuses.
    pub(crate) signatures: SignatureSubscriptionsDb,
    /// Subscriptions for transaction logs.
    pub(crate) logs: LogsSubscriptionsDb,
    /// Subscriptions for slot updates.
    pub(crate) slot: SlotSubscriptionsDb,
}

impl Default for SubscriptionsDb {
    /// Initializes the subscription databases, pre-allocating entries for global
    /// subscriptions like `logs` and `slot`.
    fn default() -> Self {
        let slot = UpdateSubscriber::new(None, SlotEncoder);
        Self {
            accounts: Default::default(),
            programs: Default::default(),
            signatures: Default::default(),
            logs: Arc::new(RwLock::new(UpdateSubscribers(Vec::new()))),
            slot: Arc::new(RwLock::new(slot)),
        }
    }
}

impl SubscriptionsDb {
    /// Subscribes a connection to receive updates for a specific account.
    ///
    /// # Returns
    /// A `SubscriptionHandle` which must be kept alive. When the handle is dropped,
    /// the client is automatically unsubscribed.
    pub(crate) async fn subscribe_to_account(
        &self,
        pubkey: Pubkey,
        encoder: AccountEncoder,
        chan: WsConnectionChannel,
    ) -> SubscriptionHandle {
        let conid = chan.id;
        let id = self
            .accounts
            .entry_async(pubkey)
            .await
            .or_insert_with(|| UpdateSubscribers(vec![]))
            .add_subscriber(chan, encoder.clone());

        // Create a cleanup future that will be executed when the handle is dropped.
        let accounts = self.accounts.clone();
        let callback = async move {
            let Some(mut entry) = accounts.get_async(&pubkey).await else {
                return;
            };
            // If this was the last subscriber for this key, remove the key from the map.
            if entry.remove_subscriber(conid, &encoder) {
                let _ = entry.remove();
            }
        };
        let cleanup = CleanUp(Some(Box::pin(callback)));
        SubscriptionHandle { id, cleanup }
    }

    /// Finds and notifies all subscribers for a given account update.
    pub(crate) async fn send_account_update(&self, update: &AccountWithSlot) {
        self.accounts
            .read_async(&update.account.pubkey, |_, subscribers| {
                subscribers.send(&update.account, update.slot)
            })
            .await;
    }

    /// Subscribes a connection to receive updates for accounts owned by a specific program.
    pub(crate) async fn subscribe_to_program(
        &self,
        pubkey: Pubkey,
        encoder: ProgramAccountEncoder,
        chan: WsConnectionChannel,
    ) -> SubscriptionHandle {
        let conid = chan.id;
        let id = self
            .programs
            .entry_async(pubkey)
            .await
            .or_insert_with(|| UpdateSubscribers(vec![]))
            .add_subscriber(chan, encoder.clone());

        let programs = self.programs.clone();
        let callback = async move {
            let Some(mut entry) = programs.get_async(&pubkey).await else {
                return;
            };
            if entry.remove_subscriber(conid, &encoder) {
                let _ = entry.remove();
            }
        };
        let cleanup = CleanUp(Some(Box::pin(callback)));
        SubscriptionHandle { id, cleanup }
    }

    /// Finds and notifies all subscribers for a given program account update.
    pub(crate) async fn send_program_update(&self, update: &AccountWithSlot) {
        let owner = update.account.account.owner();
        self.programs
            .read_async(owner, |_, subscribers| {
                subscribers.send(&update.account, update.slot)
            })
            .await;
    }

    /// Subscribes a connection to a one-shot notification for a transaction signature.
    ///
    /// This subscription is automatically removed after the first notification.
    /// The returned `AtomicBool` is used to coordinate its lifecycle with the `SignaturesExpirer`.
    pub(crate) async fn subscribe_to_signature(
        &self,
        signature: Signature,
        chan: WsConnectionChannel,
    ) -> (SubscriptionID, Arc<AtomicBool>) {
        let encoder = TransactionResultEncoder;
        let subscriber = self
            .signatures
            .entry_async(signature)
            .await
            .or_insert_with(|| UpdateSubscriber::new(Some(chan), encoder));
        (subscriber.id, subscriber.live.clone())
    }

    /// Sends a notification to a signature subscriber and removes the subscription.
    pub(crate) async fn send_signature_update(
        &self,
        signature: &Signature,
        update: &TransactionResult,
        slot: Slot,
    ) {
        // Atomically remove the subscriber to ensure it's only notified once.
        let Some((_, subscriber)) =
            self.signatures.remove_async(signature).await
        else {
            return;
        };
        subscriber.send(update, slot)
    }

    /// Subscribes a connection to receive all transaction logs.
    pub(crate) fn subscribe_to_logs(
        &self,
        encoder: TransactionLogsEncoder,
        chan: WsConnectionChannel,
    ) -> SubscriptionHandle {
        let conid = chan.id;
        let id = self.logs.write().add_subscriber(chan, encoder.clone());

        let logs = self.logs.clone();
        let callback = async move {
            logs.write().remove_subscriber(conid, &encoder);
        };
        let cleanup = CleanUp(Some(Box::pin(callback)));
        SubscriptionHandle { id, cleanup }
    }

    /// Sends a log update to all log subscribers.
    pub(crate) fn send_logs_update(
        &self,
        update: &TransactionStatus,
        slot: Slot,
    ) {
        self.logs.read().send(update, slot);
    }

    /// Subscribes a connection to receive slot updates.
    pub(crate) fn subscribe_to_slot(
        &self,
        chan: WsConnectionChannel,
    ) -> SubscriptionHandle {
        let conid = chan.id;
        let mut subscriber = self.slot.write();
        subscriber.txs.insert(chan.id, chan.tx);
        let id = subscriber.id;

        let slot = self.slot.clone();
        let callback = async move {
            slot.write().txs.remove(&conid);
        };
        let cleanup = CleanUp(Some(Box::pin(callback)));
        SubscriptionHandle { id, cleanup }
    }

    /// Sends a slot update to all slot subscribers.
    pub(crate) fn send_slot(&self, slot: Slot) {
        self.slot.read().send(&(), slot);
    }

    /// Generates the next unique subscription ID.
    pub(crate) fn next_subid() -> SubscriptionID {
        SUBID_COUNTER.fetch_add(1, Ordering::Relaxed)
    }
}

/// A collection of `UpdateSubscriber`s for a single subscription key (e.g., a specific account).
/// The inner `Vec` is kept sorted by encoder to allow for efficient lookups.
pub(crate) struct UpdateSubscribers<E>(Vec<UpdateSubscriber<E>>);

/// Represents a group of subscribers that share the same subscription ID and encoding options.
pub(crate) struct UpdateSubscriber<E> {
    /// The unique public-facing ID for this subscription.
    id: SubscriptionID,
    /// The specific encoding and configuration for notifications.
    encoder: E,
    /// A map of `ConnectionID` to a sender channel for each connected client in this group.
    txs: BTreeMap<ConnectionID, ConnectionTx>,
    /// A flag to signal if the subscription is still active. Used primarily for one-shot
    /// `signatureSubscribe` to prevent race conditions with the expiration mechanism.
    live: Arc<AtomicBool>,
}

impl<E: Encoder> UpdateSubscribers<E> {
    /// Adds a connection to the appropriate subscriber group based on the encoder.
    /// If no group exists for the given encoder, a new one is created.
    fn add_subscriber(&mut self, chan: WsConnectionChannel, encoder: E) -> u64 {
        match self.0.binary_search_by(|s| s.encoder.cmp(&encoder)) {
            // A subscriber group with this encoder already exists.
            Ok(index) => {
                let subscriber = &mut self.0[index];
                subscriber.txs.insert(chan.id, chan.tx);
                subscriber.id
            }
            // No group for this encoder, create a new one.
            Err(index) => {
                let subsriber = UpdateSubscriber::new(Some(chan), encoder);
                let id = subsriber.id;
                self.0.insert(index, subsriber);
                id
            }
        }
    }

    /// Removes a connection from a subscriber group.
    /// If the group becomes empty, it is removed from the collection.
    /// Returns `true` if the entire collection becomes empty.
    fn remove_subscriber(&mut self, conid: ConnectionID, encoder: &E) -> bool {
        let Ok(index) = self.0.binary_search_by(|s| s.encoder.cmp(encoder))
        else {
            return false;
        };
        let subscriber = &mut self.0[index];
        subscriber.txs.remove(&conid);
        if subscriber.txs.is_empty() {
            self.0.remove(index);
        }
        self.0.is_empty()
    }

    /// Sends an update to all subscriber groups in this collection.
    #[inline]
    fn send(&self, msg: &E::Data, slot: Slot) {
        for subscriber in &self.0 {
            subscriber.send(msg, slot);
        }
    }

    #[cfg(test)]
    pub(crate) fn count(&self) -> usize {
        self.0.len()
    }
}

impl<E: Encoder> UpdateSubscriber<E> {
    /// Creates a new subscriber group.
    fn new(chan: Option<WsConnectionChannel>, encoder: E) -> Self {
        let id = SubscriptionsDb::next_subid();
        let mut txs = BTreeMap::new();
        if let Some(chan) = chan {
            txs.insert(chan.id, chan.tx);
        }
        let live = AtomicBool::new(true).into();
        UpdateSubscriber {
            id,
            encoder,
            txs,
            live,
        }
    }

    /// Encodes a message and sends it to all connections in this group.
    #[inline]
    fn send(&self, msg: &E::Data, slot: Slot) {
        let Some(bytes) = self.encoder.encode(slot, msg, self.id) else {
            return;
        };
        for tx in self.txs.values() {
            // Use try_send to avoid blocking if a client's channel is full.
            let _ = tx.try_send(bytes.clone());
        }
    }

    #[cfg(test)]
    pub(crate) fn count(&self) -> usize {
        self.txs.len()
    }
}

/// A handle representing an active subscription.
///
/// Its primary purpose is to manage the subscription's lifecycle via the RAII pattern.
/// When this handle is dropped, its `cleanup` logic is automatically triggered
/// to unsubscribe the client from the database.
pub(crate) struct SubscriptionHandle {
    pub(crate) id: SubscriptionID,
    pub(crate) cleanup: CleanUp,
}

/// A RAII guard that executes an asynchronous cleanup task when dropped.
pub(crate) struct CleanUp(
    Option<Pin<Box<dyn Future<Output = ()> + Send + Sync>>>,
);

impl Drop for CleanUp {
    /// When dropped, spawns the contained future onto the Tokio runtime to perform cleanup.
    fn drop(&mut self) {
        if let Some(cb) = self.0.take() {
            tokio::spawn(cb);
        }
    }
}

impl<E> Drop for UpdateSubscriber<E> {
    /// When a signature subscriber is dropped (e.g., after being notified),
    /// this sets its `live` flag to false.
    fn drop(&mut self) {
        self.live.store(false, Ordering::Relaxed);
    }
}
