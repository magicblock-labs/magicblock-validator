use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use hyper::body::Bytes;
use magicblock_gateway_types::{
    accounts::{AccountSharedData, Pubkey},
    transactions::{TransactionResult, TransactionStatus},
};
use parking_lot::RwLock;
use solana_signature::Signature;
use tokio::sync::mpsc;

use crate::encoder::{
    AccountEncoder, Encoder, ProgramAccountEncoder, SlotEncoder,
    TransactionLogsEncoder, TransactionResultEncoder,
};

type AccountSubscriptionsDb =
    Arc<scc::HashMap<Pubkey, UpdateSubscribers<AccountEncoder>>>;
type ProgramSubscriptionsDb =
    Arc<scc::HashMap<Pubkey, UpdateSubscribers<ProgramAccountEncoder>>>;
type SignatureSubscriptionsDb =
    Arc<scc::HashMap<Signature, UpdateSubscribers<TransactionResultEncoder>>>;
type LogsSubscriptionsDb =
    Arc<RwLock<UpdateSubscribers<TransactionLogsEncoder>>>;
type SlotSubscriptionsDb = Arc<RwLock<UpdateSubscriber<SlotEncoder>>>;

pub type SubscriptionID = u64;
pub type ConnectionID = u64;
pub type Slot = u64;

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub(crate) struct SubscriptionsDb {
    accounts: AccountSubscriptionsDb,
    programs: ProgramSubscriptionsDb,
    signatures: SignatureSubscriptionsDb,
    logs: LogsSubscriptionsDb,
    slot: SlotSubscriptionsDb,
}

impl Default for SubscriptionsDb {
    fn default() -> Self {
        let accounts = Default::default();
        let programs = Default::default();
        let signatures = Default::default();
        let logs = Arc::new(RwLock::new(UpdateSubscribers(Vec::new())));
        let slot = UpdateSubscriber {
            id: ID_COUNTER.fetch_add(1, Ordering::Relaxed),
            encoder: SlotEncoder,
            txs: Default::default(),
        };
        Self {
            accounts,
            programs,
            signatures,
            logs,
            slot: Arc::new(RwLock::new(slot)),
        }
    }
}

impl SubscriptionsDb {
    pub(crate) async fn subscribe_to_account(
        &self,
        pubkey: Pubkey,
        encoder: AccountEncoder,
        tx: ConnectionTx,
    ) -> SubscriptionID {
        self.accounts
            .entry_async(pubkey)
            .await
            .or_insert_with(|| UpdateSubscribers(vec![]))
            .add_subscriber(tx, encoder)
    }

    pub(crate) async fn unsubscribe_from_account(
        &self,
        pubkey: &Pubkey,
        conid: ConnectionID,
        encoder: &AccountEncoder,
    ) {
        let Some(mut entry) = self.accounts.get_async(pubkey).await else {
            return;
        };
        if entry.remove_subscriber(conid, encoder) {
            drop(entry);
            self.accounts.remove_async(pubkey).await;
        }
    }

    pub(crate) async fn send_account_update(
        &self,
        update: (Pubkey, AccountSharedData),
        slot: Slot,
    ) {
        self.accounts
            .read_async(&update.0, |_, subscribers| {
                subscribers.send(&update, slot)
            })
            .await;
    }

    pub(crate) async fn subscribe_to_program(
        &self,
        pubkey: Pubkey,
        encoder: ProgramAccountEncoder,
        tx: ConnectionTx,
    ) -> SubscriptionID {
        self.programs
            .entry_async(pubkey)
            .await
            .or_insert_with(|| UpdateSubscribers(vec![]))
            .add_subscriber(tx, encoder)
    }

    pub(crate) async fn unsubscribe_from_program(
        &self,
        pubkey: &Pubkey,
        conid: ConnectionID,
        encoder: &ProgramAccountEncoder,
    ) {
        let Some(mut entry) = self.programs.get_async(pubkey).await else {
            return;
        };
        if entry.remove_subscriber(conid, encoder) {
            drop(entry);
            self.accounts.remove_async(pubkey).await;
        }
    }

    pub(crate) async fn send_program_update(
        &self,
        update: (Pubkey, AccountSharedData),
        slot: Slot,
    ) {
        self.programs
            .read_async(&update.0, |_, subscribers| {
                subscribers.send(&update, slot)
            })
            .await;
    }

    pub(crate) async fn subscribe_to_signature(
        &self,
        signature: Signature,
        tx: ConnectionTx,
    ) -> SubscriptionID {
        let encoder = TransactionResultEncoder;
        self.signatures
            .entry_async(signature)
            .await
            .or_insert_with(|| UpdateSubscribers(vec![]))
            .add_subscriber(tx, encoder)
    }

    pub(crate) async fn unsubscribe_from_signature(
        &self,
        signature: &Signature,
        conid: ConnectionID,
    ) {
        let Some(mut entry) = self.signatures.get_async(signature).await else {
            return;
        };

        if entry.remove_subscriber(conid, &TransactionResultEncoder) {
            drop(entry);
            self.signatures.remove_async(signature).await;
        }
    }

    pub(crate) async fn send_signature_update(
        &self,
        signature: &Signature,
        update: &TransactionResult,
        slot: Slot,
    ) {
        self.signatures
            .read_async(signature, |_, subscribers| {
                subscribers.send(update, slot)
            })
            .await;
    }

    pub(crate) fn subscribe_to_logs(
        &self,
        tx: ConnectionTx,
        encoder: TransactionLogsEncoder,
    ) -> SubscriptionID {
        self.logs.write().add_subscriber(tx, encoder)
    }

    pub(crate) fn unsubscribe_from_logs(
        &self,
        conid: ConnectionID,
        encoder: &TransactionLogsEncoder,
    ) {
        self.logs.write().remove_subscriber(conid, encoder);
    }

    pub(crate) fn send_logs_update(
        &self,
        update: &TransactionStatus,
        slot: Slot,
    ) {
        let subscribers = self.logs.read();
        subscribers.send(update, slot);
    }

    pub(crate) fn subscribe_to_slot(&self, tx: ConnectionTx) -> SubscriptionID {
        let mut subscriber = self.slot.write();
        subscriber.txs.push(tx);
        subscriber.id
    }

    pub(crate) fn unsubscribe_from_slot(&self, conid: ConnectionID) {
        let mut subscriber = self.slot.write();
        subscriber.txs.retain(|s| s.conid != conid);
    }

    pub(crate) fn send_slot(&self, slot: Slot) {
        let subscriber = self.slot.read();
        subscriber.send(&(), slot);
    }
}

/// Sender handles to subscribers for a given update
struct UpdateSubscribers<E>(Vec<UpdateSubscriber<E>>);

struct UpdateSubscriber<E> {
    id: SubscriptionID,
    encoder: E,
    txs: Vec<ConnectionTx>,
}

#[derive(Clone)]
pub(crate) struct ConnectionTx {
    pub conid: ConnectionID,
    pub tx: mpsc::Sender<Bytes>,
}

impl<E: Encoder> UpdateSubscribers<E> {
    fn add_subscriber(&mut self, tx: ConnectionTx, encoder: E) -> u64 {
        match self.0.binary_search_by(|s| s.encoder.cmp(&encoder)) {
            Ok(index) => {
                let subscriber = &mut self.0[index];
                subscriber.txs.push(tx);
                subscriber.id
            }
            Err(index) => {
                let id = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
                let subsriber = UpdateSubscriber {
                    id,
                    encoder,
                    txs: vec![tx],
                };
                self.0.insert(index, subsriber);
                id
            }
        }
    }

    fn remove_subscriber(&mut self, conid: ConnectionID, encoder: &E) -> bool {
        let Ok(index) = self.0.binary_search_by(|s| s.encoder.cmp(&encoder))
        else {
            return false;
        };
        let subscriber = &mut self.0[index];
        subscriber.txs.retain(|tx| tx.conid != conid);
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
    #[inline]
    fn send(&self, msg: &E::Data, slot: Slot) {
        let Some(bytes) = self.encoder.encode(slot, msg, self.id) else {
            return;
        };
        for tx in &self.txs {
            let _ = tx.tx.try_send(bytes.clone());
        }
    }
}
