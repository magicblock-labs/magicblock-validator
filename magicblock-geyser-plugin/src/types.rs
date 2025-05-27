use std::collections::HashMap;
use std::sync::Arc;

use log::warn;
use scc::hash_map::Entry;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Signature;
use tokio::sync::mpsc;

use crate::grpc_messages::{Message, MessageBlockMeta};

pub type GeyserMessage = Arc<Message>;
pub type GeyserMessages = Arc<Vec<GeyserMessage>>;
pub type GeyserMessageBlockMeta = Arc<MessageBlockMeta>;
pub type AccountSubscriptionsDb = Arc<scc::HashMap<Pubkey, UpdateSubscribers>>;
pub type ProgramSubscriptionsDb = Arc<scc::HashMap<Pubkey, UpdateSubscribers>>;
pub type SignatureSubscriptionsDb =
    Arc<scc::HashMap<Signature, UpdateSubscribers>>;
pub type LogsSubscriptionsDb =
    Arc<scc::HashMap<LogsSubscribeKey, UpdateSubscribers>>;
pub type SlotSubscriptionsDb =
    Arc<scc::HashMap<u64, mpsc::Sender<GeyserMessage>>>;

#[derive(Clone, Default)]
pub struct SubscriptionsDb {
    accounts: AccountSubscriptionsDb,
    programs: ProgramSubscriptionsDb,
    signatures: SignatureSubscriptionsDb,
    logs: LogsSubscriptionsDb,
    slot: SlotSubscriptionsDb,
}

macro_rules! add_subscriber {
    ($root: ident, $db: ident, $id: ident, $key: ident, $tx: expr) => {
        let subscriber = UpdateSubscribers::Single { id: $id, tx: $tx };
        match $root.$db.entry_async($key).await {
            Entry::Vacant(e) => {
                e.insert_entry(subscriber);
            }
            Entry::Occupied(mut e) => {
                e.add_subscriber($id, subscriber);
            }
        };
    };
}

macro_rules! remove_subscriber {
    ($root: ident, $db: ident, $id: ident, $key: ident) => {
        let Some(mut entry) = $root.$db.get_async($key).await else {
            return;
        };
        if entry.remove_subscriber($id) {
            drop(entry);
            $root.$db.remove_async($key).await;
        }
    };
}

macro_rules! send_update {
    ($root: ident, $db: ident, $key: ident, $update: ident) => {
        $root
            .$db
            .read_async($key, |_, subscribers| subscribers.send($update))
            .await;
    };
}

impl SubscriptionsDb {
    pub async fn subscribe_to_account(
        &self,
        pubkey: Pubkey,
        tx: mpsc::Sender<GeyserMessage>,
        id: u64,
    ) {
        add_subscriber!(self, accounts, id, pubkey, tx);
    }

    pub async fn unsubscribe_from_account(&self, pubkey: &Pubkey, id: u64) {
        remove_subscriber!(self, accounts, id, pubkey);
    }

    pub async fn send_account_update(
        &self,
        pubkey: &Pubkey,
        update: GeyserMessage,
    ) {
        send_update!(self, accounts, pubkey, update);
    }

    pub async fn subscribe_to_program(
        &self,
        pubkey: Pubkey,
        tx: mpsc::Sender<GeyserMessage>,
        id: u64,
    ) {
        add_subscriber!(self, programs, id, pubkey, tx);
    }

    pub async fn unsubscribe_from_program(&self, pubkey: &Pubkey, id: u64) {
        remove_subscriber!(self, programs, id, pubkey);
    }

    pub async fn send_program_update(
        &self,
        pubkey: &Pubkey,
        update: GeyserMessage,
    ) {
        send_update!(self, programs, pubkey, update);
    }

    pub async fn subscribe_to_signature(
        &self,
        signature: Signature,
        tx: mpsc::Sender<GeyserMessage>,
        id: u64,
    ) {
        add_subscriber!(self, signatures, id, signature, tx);
    }

    pub async fn unsubscribe_from_signature(
        &self,
        signature: &Signature,
        id: u64,
    ) {
        remove_subscriber!(self, signatures, id, signature);
    }

    pub async fn send_signature_update(
        &self,
        signature: &Signature,
        update: GeyserMessage,
    ) {
        send_update!(self, signatures, signature, update);
    }

    pub async fn subscribe_to_logs(
        &self,
        key: LogsSubscribeKey,
        tx: mpsc::Sender<GeyserMessage>,
        id: u64,
    ) {
        add_subscriber!(self, logs, id, key, tx);
    }

    pub async fn unsubscribe_from_logs(&self, key: &LogsSubscribeKey, id: u64) {
        remove_subscriber!(self, logs, id, key);
    }

    pub async fn send_logs_update(&self, update: GeyserMessage) {
        if self.logs.is_empty() {
            return;
        }
        let Message::Transaction(ref txn) = *update else {
            return;
        };
        let addresses = &txn.transaction.transaction.message().account_keys();
        self.logs
            .scan_async(|key, subscribers| match key {
                LogsSubscribeKey::All => {
                    subscribers.send(update.clone());
                }
                LogsSubscribeKey::Account(pubkey) => {
                    for pk in addresses.iter() {
                        if pubkey == pk {
                            subscribers.send(update.clone());
                            return;
                        }
                    }
                }
            })
            .await;
    }

    pub async fn subscribe_to_slot(
        &self,
        tx: mpsc::Sender<GeyserMessage>,
        id: u64,
    ) {
        let _ = self.slot.insert_async(id, tx).await;
    }

    pub async fn unsubscribe_from_slot(&self, id: u64) {
        self.slot.remove_async(&id).await;
    }

    pub async fn send_slot(&self, msg: GeyserMessage) {
        self.slot
            .scan_async(|_, tx| {
                if tx.try_send(msg.clone()).is_err() {
                    warn!("slot subscriber hang up or not keeping up");
                }
            })
            .await;
    }
}

pub type GeyserMessageSender = flume::Sender<GeyserMessage>;
pub type GeyserMessageReceiver = flume::Receiver<GeyserMessage>;

pub fn geyser_message_channel() -> (GeyserMessageSender, GeyserMessageReceiver)
{
    flume::unbounded()
}

#[derive(Hash, PartialEq, Eq, Clone, Copy, Debug)]
pub enum LogsSubscribeKey {
    All,
    Account(Pubkey),
}

/// Sender handles to subscribers for a given update
pub enum UpdateSubscribers {
    Single {
        id: u64,
        tx: mpsc::Sender<GeyserMessage>,
    },
    Multiple(HashMap<u64, Self>),
}

impl UpdateSubscribers {
    /// Adds the subscriber to the list, upgrading Self to Multiple if necessary
    fn add_subscriber(&mut self, id: u64, subscriber: Self) {
        if let Self::Multiple(txs) = self {
            txs.insert(id, subscriber);
            return;
        }
        let mut txs = HashMap::with_capacity(2);
        txs.insert(id, subscriber);
        let multiple = Self::Multiple(txs);
        let previous = std::mem::replace(self, multiple);
        if let Self::Single { id, .. } = previous {
            self.add_subscriber(id, previous);
        }
    }

    /// Checks whether there're multiple subscribers, if so, removes the
    /// specified one, returns a boolean indicating whether or not more
    /// subscribers are left. For Oneshot and Single always returns true
    fn remove_subscriber(&mut self, id: u64) -> bool {
        if let Self::Multiple(txs) = self {
            txs.remove(&id);
            txs.is_empty()
        } else {
            true
        }
    }

    /// Sends the update message to all existing subscribers/handlers
    fn send(&self, msg: GeyserMessage) {
        match self {
            Self::Single { tx, .. } => {
                if tx.try_send(msg).is_err() {
                    warn!("mpsc update receiver hang up or not keeping up");
                }
            }
            Self::Multiple(txs) => {
                for tx in txs.values() {
                    tx.send(msg.clone());
                }
            }
        }
    }
}
