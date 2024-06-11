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
    current_id: Pubkey,
) -> Result<oneshot::Receiver<TriggerCommitResult>, MagicErrorWithContext> {
    let commit_sender_lock =
        COMMIT_SENDER.read().expect("RwLock COMMIT_SENDER poisoned");

    let commit_sender = commit_sender_lock.as_ref().ok_or_else(|| {
        MagicErrorWithContext::new(
            MagicError::InternalError,
            "Commit sender needs to be set at startup".to_string(),
        )
    })?;

    let (current_sender, current_receiver) = oneshot::channel();
    commit_sender
        .blocking_send((current_id, current_sender))
        .map_err(|err| {
            MagicErrorWithContext::new(
                MagicError::InternalError,
                format!("Failed to send commit pubkey: {}", err),
            )
        })?;

    Ok(current_receiver)
}

/// The below methods are needed to allow multiple tests to run in parallel sharing one commit channel.
/// The send/recv messages are routed to each registered test.
#[cfg(feature = "dev-context-only-utils")]
mod test_utils {
    use std::{collections::HashSet, sync::RwLock};

    use super::*;

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
                println!(">>>>>>>> commit_sender AFTER SPAWN");
                while let Some((current_id, current_sender)) =
                    commit_receiver.recv().await
                {
                    println!(
                        ">>>>>>>> commit_sender AFTER RECV: {}",
                        current_id
                    );
                    if COMMIT_ROUTING_KEYS
                        .read()
                        .expect("RwLock COMMIT_HANDLE poisoned")
                        .contains(&current_id)
                    {
                        println!(
                            ">>>>>>>> commit_sender ALLOWED: {}",
                            current_id
                        );
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
                        println!(
                            ">>>>>>>> commit_sender ERROR: {}",
                            current_id
                        );
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
                println!(">>>>>>>> commit_sender AFTER LOOOP!!");
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
