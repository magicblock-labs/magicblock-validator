use std::collections::HashMap;

use log::error;
use magicblock_program::magic_scheduled_l1_message::{
    CommittedAccountV2, MagicL1Message, ScheduledL1Message,
};
use solana_pubkey::Pubkey;

use crate::{
    persist::{CommitStatus, L1MessagesPersisterIface},
    tasks::tasks::TaskPreparationInfo,
};

pub trait ScheduledMessageExt {
    fn get_committed_accounts(&self) -> Option<&Vec<CommittedAccountV2>>;
    fn get_committed_pubkeys(&self) -> Option<Vec<Pubkey>>;

    // TODO(edwin): ugly
    fn is_undelegate(&self) -> bool;
}

impl ScheduledMessageExt for ScheduledL1Message {
    fn get_committed_accounts(&self) -> Option<&Vec<CommittedAccountV2>> {
        match &self.l1_message {
            MagicL1Message::L1Actions(_) => None,
            MagicL1Message::Commit(t) => Some(t.get_committed_accounts()),
            MagicL1Message::CommitAndUndelegate(t) => {
                Some(t.get_committed_accounts())
            }
        }
    }

    fn get_committed_pubkeys(&self) -> Option<Vec<Pubkey>> {
        self.get_committed_accounts().map(|accounts| {
            accounts.iter().map(|account| account.pubkey).collect()
        })
    }

    fn is_undelegate(&self) -> bool {
        match &self.l1_message {
            MagicL1Message::L1Actions(_) => false,
            MagicL1Message::Commit(_) => false,
            MagicL1Message::CommitAndUndelegate(_) => true,
        }
    }
}

pub(crate) fn persist_status_update<P: L1MessagesPersisterIface>(
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

pub(crate) fn persist_status_update_set<P: L1MessagesPersisterIface>(
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
