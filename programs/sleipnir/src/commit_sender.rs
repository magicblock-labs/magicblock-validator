use std::sync::RwLock;

use lazy_static::lazy_static;
use solana_sdk::{pubkey::Pubkey, signature::Signature};
use tokio::sync::{mpsc, oneshot};

use crate::errors::{MagicError, MagicErrorWithContext};

pub type TriggerCommitResult = Result<Signature, MagicErrorWithContext>;
pub type TriggerCommitCallback = oneshot::Sender<TriggerCommitResult>;
pub type TriggerCommitSender = mpsc::Sender<(Pubkey, TriggerCommitCallback)>;
pub type TriggerCommitReceiver =
    mpsc::Receiver<(Pubkey, TriggerCommitCallback)>;

lazy_static! {
    static ref COMMIT_SENDER: RwLock<Option<TriggerCommitSender>> =
        RwLock::new(None);
}

pub fn init_commit_channel(buffer: usize) -> TriggerCommitReceiver {
    let (tx, rx) = mpsc::channel(buffer);
    set_commit_sender(tx);
    rx
}

pub fn send_commit(
    pubkey: Pubkey,
) -> Result<oneshot::Receiver<TriggerCommitResult>, MagicErrorWithContext> {
    let sender_lock =
        COMMIT_SENDER.read().expect("RwLock COMMIT_SENDER poisoned");

    let sender = sender_lock.as_ref().ok_or_else(|| {
        MagicErrorWithContext::new(
            MagicError::InternalError,
            "Commit sender needs to be set at startup".to_string(),
        )
    })?;

    let (tx, rx) = oneshot::channel();
    sender.blocking_send((pubkey, tx)).map_err(|err| {
        MagicErrorWithContext::new(
            MagicError::InternalError,
            format!("Failed to send commit pubkey: {}", err),
        )
    })?;
    Ok(rx)
}

pub fn has_sender() -> bool {
    COMMIT_SENDER
        .read()
        .expect("RwLock COMMIT_SENDER poisoned")
        .is_some()
}

fn set_commit_sender(sender: mpsc::Sender<(Pubkey, TriggerCommitCallback)>) {
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
