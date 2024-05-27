use std::sync::RwLock;

use lazy_static::lazy_static;
use solana_sdk::pubkey::Pubkey;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use crate::errors::MagicErrorWithContext;

pub type TriggerCommitResult = Result<(), MagicErrorWithContext>;
pub type TriggerCommitCallback = oneshot::Sender<TriggerCommitResult>;

lazy_static! {
    static ref COMMIT_SENDER: RwLock<Option<mpsc::Sender<(Pubkey, TriggerCommitCallback)>>> =
        RwLock::new(None);
}

pub fn send_commit(pubkey: Pubkey) -> oneshot::Receiver<TriggerCommitResult> {
    let sender_lock =
        COMMIT_SENDER.read().expect("RwLock COMMIT_SENDER poisoned");
    let sender = sender_lock
        .as_ref()
        .expect("Commit sender needs to be set on startup");

    let (tx, rx) = oneshot::channel();
    sender
        .blocking_send((pubkey, tx))
        .expect("Failed to send commit pubkey");
    rx
}

pub fn has_sender() -> bool {
    COMMIT_SENDER
        .read()
        .expect("RwLock COMMIT_SENDER poisoned")
        .is_some()
}

pub fn set_commit_sender(
    sender: mpsc::Sender<(Pubkey, TriggerCommitCallback)>,
) {
    {
        let sender =
            COMMIT_SENDER.read().expect("RwLock COMMIT_SENDER poisoned");

        if sender.is_some() {
            panic!("Commit sender can only be set once, but was set before",);
        }
    }

    COMMIT_SENDER
        .write()
        .expect("RwLock COMMIT_SENDER poisoned")
        .replace(sender);
}
