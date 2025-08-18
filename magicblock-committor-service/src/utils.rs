use std::collections::HashMap;

use log::error;
use solana_pubkey::Pubkey;

use crate::persist::{CommitStatus, IntentPersister};

pub(crate) fn persist_status_update<P: IntentPersister>(
    persister: &Option<P>,
    pubkey: &Pubkey,
    commit_id: u64,
    update_status: CommitStatus,
) {
    let Some(persister) = persister else {
        return;
    };
    if let Err(err) = persister.update_status_by_message(
        commit_id,
        pubkey,
        update_status.clone(),
    ) {
        error!("Failed to persist new status {}: {}", update_status, err);
    }
}

#[allow(unused)]
pub(crate) fn persist_status_update_set<P: IntentPersister>(
    persister: &Option<P>,
    commit_ids_map: &HashMap<Pubkey, u64>,
    update_status: CommitStatus,
) {
    let Some(persister) = persister else {
        return;
    };
    commit_ids_map.iter().for_each(|(pubkey, commit_id)| {
        if let Err(err) = persister.update_status_by_commit(
            *commit_id,
            pubkey,
            update_status.clone(),
        ) {
            error!("Failed to persist new status {}: {}", update_status, err);
        }
    });
}

/// Persists update by message/intent id
pub(crate) fn persist_status_update_by_message_set<P: IntentPersister>(
    persister: &P,
    message_id: u64,
    pubkeys: &[Pubkey],
    update_status: CommitStatus,
) {
    pubkeys.iter().for_each(|pubkey| {
        if let Err(err) = persister.update_status_by_message(
            message_id,
            pubkey,
            update_status.clone(),
        ) {
            error!("Failed to persist new status {}: {}", update_status, err);
        }
    });
}
