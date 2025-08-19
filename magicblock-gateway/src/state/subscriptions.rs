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
    Slot,
};
use magicblock_core::link::{
    accounts::AccountWithSlot,
    transactions::{TransactionResult, TransactionStatus},
};

pub(crate) type AccountSubscriptionsDb =
    Arc<scc::HashMap<Pubkey, UpdateSubscribers<AccountEncoder>>>;
pub(crate) type ProgramSubscriptionsDb =
    Arc<scc::HashMap<Pubkey, UpdateSubscribers<ProgramAccountEncoder>>>;
pub(crate) type SignatureSubscriptionsDb =
    Arc<scc::HashMap<Signature, UpdateSubscriber<TransactionResultEncoder>>>;
pub(crate) type LogsSubscriptionsDb =
    Arc<RwLock<UpdateSubscribers<TransactionLogsEncoder>>>;
pub(crate) type SlotSubscriptionsDb =
    Arc<RwLock<UpdateSubscriber<SlotEncoder>>>;

pub(crate) type SubscriptionID = u64;

static SUBID_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub(crate) struct SubscriptionsDb {
    pub(crate) accounts: AccountSubscriptionsDb,
    pub(crate) programs: ProgramSubscriptionsDb,
    pub(crate) signatures: SignatureSubscriptionsDb,
    pub(crate) logs: LogsSubscriptionsDb,
    pub(crate) slot: SlotSubscriptionsDb,
}

impl Default for SubscriptionsDb {
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
        let accounts = self.accounts.clone();
        let callback = async move {
            if let Some(mut entry) = accounts.get_async(&pubkey).await {
                entry
                    .remove_subscriber(conid, &encoder)
                    .then(|| entry.remove());
            };
        };
        let cleanup = CleanUp(Some(Box::pin(callback)));
        SubscriptionHandle { id, cleanup }
    }

    pub(crate) async fn send_account_update(&self, update: &AccountWithSlot) {
        self.accounts
            .read_async(&update.account.pubkey, |_, subscribers| {
                subscribers.send(&update.account, update.slot)
            })
            .await;
    }

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
            if let Some(mut entry) = programs.get_async(&pubkey).await {
                entry
                    .remove_subscriber(conid, &encoder)
                    .then(|| entry.remove());
            };
        };
        let cleanup = CleanUp(Some(Box::pin(callback)));
        SubscriptionHandle { id, cleanup }
    }

    pub(crate) async fn send_program_update(&self, update: &AccountWithSlot) {
        let owner = update.account.account.owner();
        self.programs
            .read_async(owner, |_, subscribers| {
                subscribers.send(&update.account, update.slot)
            })
            .await;
    }

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

    pub(crate) async fn send_signature_update(
        &self,
        signature: &Signature,
        update: &TransactionResult,
        slot: Slot,
    ) {
        let Some((_, subscriber)) =
            self.signatures.remove_async(signature).await
        else {
            return;
        };
        subscriber.send(update, slot)
    }

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

    pub(crate) fn send_logs_update(
        &self,
        update: &TransactionStatus,
        slot: Slot,
    ) {
        let subscribers = self.logs.read();
        subscribers.send(update, slot);
    }

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
            let mut subscriber = slot.write();
            subscriber.txs.remove(&conid);
        };
        let cleanup = CleanUp(Some(Box::pin(callback)));
        SubscriptionHandle { id, cleanup }
    }

    pub(crate) fn send_slot(&self, slot: Slot) {
        let subscriber = self.slot.read();
        subscriber.send(&(), slot);
    }

    pub(crate) fn next_subid() -> SubscriptionID {
        SUBID_COUNTER.fetch_add(1, Ordering::Relaxed)
    }
}

/// Sender handles to subscribers for a given update
pub(crate) struct UpdateSubscribers<E>(Vec<UpdateSubscriber<E>>);

pub(crate) struct UpdateSubscriber<E> {
    id: SubscriptionID,
    encoder: E,
    txs: BTreeMap<ConnectionID, ConnectionTx>,
    live: Arc<AtomicBool>,
}

impl<E: Encoder> UpdateSubscribers<E> {
    fn add_subscriber(&mut self, chan: WsConnectionChannel, encoder: E) -> u64 {
        match self.0.binary_search_by(|s| s.encoder.cmp(&encoder)) {
            Ok(index) => {
                let subscriber = &mut self.0[index];
                subscriber.txs.insert(chan.id, chan.tx);
                subscriber.id
            }
            Err(index) => {
                let subsriber = UpdateSubscriber::new(Some(chan), encoder);
                let id = subsriber.id;
                self.0.insert(index, subsriber);
                id
            }
        }
    }

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

    /// Sends the update message to all existing subscribers/handlers
    #[inline]
    fn send(&self, msg: &E::Data, slot: Slot) {
        for subscriber in &self.0 {
            subscriber.send(msg, slot);
        }
    }
}

impl<E: Encoder> UpdateSubscriber<E> {
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

    #[inline]
    fn send(&self, msg: &E::Data, slot: Slot) {
        let Some(bytes) = self.encoder.encode(slot, msg, self.id) else {
            return;
        };
        for tx in self.txs.values() {
            let _ = tx.try_send(bytes.clone());
        }
    }
}

pub(crate) struct SubscriptionHandle {
    pub(crate) id: SubscriptionID,
    pub(crate) cleanup: CleanUp,
}

pub(crate) struct CleanUp(
    Option<Pin<Box<dyn Future<Output = ()> + Send + Sync>>>,
);

impl Drop for CleanUp {
    fn drop(&mut self) {
        if let Some(cb) = self.0.take() {
            tokio::spawn(cb);
        }
    }
}

impl<E> Drop for UpdateSubscriber<E> {
    fn drop(&mut self) {
        self.live.store(false, Ordering::Relaxed);
    }
}
