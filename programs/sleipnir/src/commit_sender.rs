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

enum InitChannelResult {
    AlreadyInitialized,
    InitializedReceiver(TriggerCommitReceiver),
}

lazy_static! {
    static ref COMMIT_SENDER: RwLock<Option<TriggerCommitSender>> =
        RwLock::new(None);
}

/// This will check if there is a commit channel initialized already and only initialize
/// one if there wasn't, otherwise it returns [InitChannelResult::AlreadyInitialized].
fn ensure_commit_channel(buffer: usize) -> InitChannelResult {
    use InitChannelResult::*;
    let mut commit_sender_lock = COMMIT_SENDER
        .write()
        .expect("RwLock COMMIT_HANDLE poisoned");
    if commit_sender_lock.is_some() {
        return AlreadyInitialized;
    }
    let (commit_sender, commit_receiver) = mpsc::channel(buffer);
    commit_sender_lock.replace(commit_sender);
    InitializedReceiver(commit_receiver)
}

pub fn init_commit_channel(buffer: usize) -> TriggerCommitReceiver {
    use InitChannelResult::*;
    match ensure_commit_channel(buffer) {
        AlreadyInitialized => {
            panic!("Commit sender can only be set once, but was set before")
        }
        InitializedReceiver(commit_receiver) => commit_receiver,
    }
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

#[cfg(feature = "dev-context-only-utils")]
mod test_utils {
    use super::*;
    /// The below methods are needed to allow multiple tests to run in parallel sharing one commit
    /// channel.
    /// The send/recv messages are routed to each registered test.
    use std::{collections::HashSet, sync::RwLock};

    lazy_static! {
        static ref COMMIT_ROUTING_KEYS: RwLock<HashSet<Pubkey>> =
            RwLock::new(HashSet::new());
    }
    /// This function can be called multiple time, but ensures to only create one commit channel and
    /// spawn one tokio task handling the incoming commits which get routed by id.
    pub fn ensure_routing_commit_channel(buffer: usize) {
        use InitChannelResult::*;
        if let InitializedReceiver(mut commit_receiver) =
            ensure_commit_channel(buffer)
        {
            tokio::task::spawn(async move {
                while let Some((current_id, current_sender)) =
                    commit_receiver.recv().await
                {
                    if COMMIT_ROUTING_KEYS
                        .read()
                        .expect("RwLock COMMIT_HANDLE poisoned")
                        .contains(&current_id)
                    {
                        let _ = current_sender
                            .send(Ok(Signature::default()))
                            .map_err(|err| {
                                eprintln!(
                                    "Failed to send commit result: {:?}",
                                    err
                                );
                                err
                            });
                    } else {
                        let _ = current_sender
                            .send(Err(MagicErrorWithContext::new(
                                MagicError::AccountNotDelegated,
                                format!(
                                    "Unknown commit channel key received: '{}'",
                                    current_id
                                ),
                            )))
                            .map_err(|err| {
                                eprintln!(
                                    "Failed to send commit error: {:?}",
                                    err
                                );
                                err
                            });
                    }
                }
            });
        }
    }

    pub fn add_commit_channel_route(handled_id: &Pubkey) {
        COMMIT_ROUTING_KEYS
            .write()
            .expect("RwLock COMMIT_HANDLE poisoned")
            .insert(*handled_id);
    }
}

#[cfg(feature = "dev-context-only-utils")]
pub use test_utils::*;
