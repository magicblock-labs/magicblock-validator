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
    static ref COMMIT_HANDLED_KEYS: RwLock<HashSet<Pubkey>> =
        RwLock::new(HashSet::new());
}

fn init_commit_channel_if_possible(
    buffer: usize,
) -> Option<TriggerCommitReceiver> {
    let mut commit_sender_lock = COMMIT_SENDER
        .write()
        .expect("RwLock COMMIT_HANDLE poisoned");
    if commit_sender_lock.is_some() {
        return None;
    }
    let (commit_sender, commit_receiver) = mpsc::channel(buffer);
    commit_sender_lock.replace(commit_sender);
    Some(commit_receiver)
}

pub fn init_commit_channel_as_channel(buffer: usize) -> TriggerCommitReceiver {
    if let Some(commit_receiver) = init_commit_channel_if_possible(buffer) {
        return commit_receiver;
    }
    panic!("Commit sender can only be set once, but was set before",);
}

#[cfg(feature = "dev-context-only-utils")]
pub fn init_commit_channel_as_handled_map_if_needed(buffer: usize) {
    if let Some(mut commit_receiver) = init_commit_channel_if_possible(buffer) {
        tokio::task::spawn(async move {
            while let Some((current_id, current_sender)) =
                commit_receiver.recv().await
            {
                if COMMIT_HANDLED_KEYS
                    .read()
                    .expect("RwLock COMMIT_HANDLE poisoned")
                    .contains(&current_id)
                {
                    let _ = current_sender.send(Ok(Signature::default()));
                } else {
                    let _ =
                        current_sender.send(Err(MagicErrorWithContext::new(
                            MagicError::AccountNotDelegated,
                            format!(
                                "Unknown commit channel key received: '{}'",
                                current_id
                            ),
                        )));
                }
            }
        });
    }
}

#[cfg(feature = "dev-context-only-utils")]
pub fn setup_commit_channel_handled_map_key(handled_id: &Pubkey) {
    COMMIT_HANDLED_KEYS
        .write()
        .expect("RwLock COMMIT_HANDLE poisoned")
        .insert(*handled_id);
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
