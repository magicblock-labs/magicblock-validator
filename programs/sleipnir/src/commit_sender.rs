use std::{collections::HashSet, sync::RwLock};

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
    static ref COMMIT_ALLOWS: RwLock<HashSet<Pubkey>> =
        RwLock::new(HashSet::new());
}

pub fn init_commit_channel_as_receiver(buffer: usize) -> TriggerCommitReceiver {
    let mut commit_sender_lock = COMMIT_SENDER
        .write()
        .expect("RwLock COMMIT_SENDER poisoned");
    if commit_sender_lock.is_some() {
        panic!("Commit sender can only be set once, but was set before",);
    }

    let (commit_sender, commit_receiver) = mpsc::channel(buffer);
    commit_sender_lock.replace(commit_sender);

    commit_receiver
}

pub fn init_commit_channel_as_handlers_if_needed(buffer: usize) {
    let mut commit_sender_lock = COMMIT_SENDER
        .write()
        .expect("RwLock COMMIT_ALLOWS poisoned");
    if commit_sender_lock.is_some() {
        return;
    }

    let (commit_sender, commit_receiver) = mpsc::channel(buffer);
    commit_sender_lock.replace(commit_sender);

    let mut commit_receiver = commit_receiver;

    tokio::task::spawn(async move {
        while let Some((current_id, current_sender)) =
            commit_receiver.recv().await
        {
            if COMMIT_ALLOWS
                .read()
                .expect("RwLock COMMIT_ALLOWS poisoned")
                .contains(&current_id)
            {
                let _ = current_sender.send(Ok(Signature::default()));
            } else {
                let _ = current_sender.send(Err(MagicErrorWithContext::new(
                    MagicError::AccountNotDelegated,
                    format!("Handler undefined for: '{}'", current_id),
                )));
            }
        }
    });
}

pub fn setup_commit_channel_handler(handler_id: &Pubkey) {
    COMMIT_ALLOWS
        .write()
        .expect("RwLock COMMIT_ALLOWS poisoned")
        .insert(*handler_id);
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
