use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use magicblock_program::magic_scheduled_l1_message::ScheduledL1Message;
use solana_sdk::pubkey::Pubkey;

use super::{
    db::CommitStatusRow, error::CommitPersistResult, utils::now, CommitStatus,
    CommitStrategy, CommitType, CommittsDb, MessageSignatures,
};
use crate::utils::ScheduledMessageExt;

const POISONED_MUTEX_MSG: &str = "Commitor Persister lock poisoned";

/// Records lifespan pf L1Message
pub trait L1MessagesPersisterIface: Send + Sync + Clone + 'static {
    /// Starts persisting L1Message
    fn start_l1_messages(
        &self,
        l1_message: &[ScheduledL1Message],
    ) -> CommitPersistResult<()>;
    fn start_l1_message(
        &self,
        l1_message: &ScheduledL1Message,
    ) -> CommitPersistResult<()>;
    fn set_commit_id(
        &self,
        message_id: u64,
        pubkey: &Pubkey,
        commit_id: u64,
    ) -> CommitPersistResult<()>;
    fn set_commit_strategy(
        &self,
        commit_id: u64,
        pubkey: &Pubkey,
        value: CommitStrategy,
    ) -> CommitPersistResult<()>;
    fn update_status_by_message(
        &self,
        message_id: u64,
        pubkey: &Pubkey,
        status: CommitStatus,
    ) -> CommitPersistResult<()>;
    fn update_status_by_commit(
        &self,
        commit_id: u64,
        pubkey: &Pubkey,
        status: CommitStatus,
    ) -> CommitPersistResult<()>;
    fn get_commit_statuses_by_message(
        &self,
        message_id: u64,
    ) -> CommitPersistResult<Vec<CommitStatusRow>>;
    fn get_commit_status_by_message(
        &self,
        message_id: u64,
        pubkey: &Pubkey,
    ) -> CommitPersistResult<Option<CommitStatusRow>>;
    fn get_signatures_by_commit(
        &self,
        commit_id: u64,
        pubkey: &Pubkey,
    ) -> CommitPersistResult<Option<MessageSignatures>>;
    // fn finalize_l1_message(&self blockhash: Hash) -> CommitPersistResult<()>;
}

#[derive(Clone)]
pub struct L1MessagePersister {
    // DB that tracks lifespan of Commit intents
    commits_db: Arc<Mutex<CommittsDb>>,
    // TODO(edwin): something like
    // actions_db: Arc<Mutex<ActionsDb>>
}

impl L1MessagePersister {
    pub fn try_new<P>(db_file: P) -> CommitPersistResult<Self>
    where
        P: AsRef<Path>,
    {
        let db = CommittsDb::new(db_file)?;
        db.create_commit_status_table()?;

        Ok(Self {
            commits_db: Arc::new(Mutex::new(db)),
        })
    }

    fn create_commit_rows(
        l1_message: &ScheduledL1Message,
    ) -> Vec<CommitStatusRow> {
        let Some(committed_accounts) = l1_message.get_committed_accounts()
        else {
            // We don't persist standalone actions
            return vec![];
        };

        let undelegate = l1_message.is_undelegate();
        let created_at = now();
        committed_accounts
            .iter()
            .map(|account| {
                let data = &account.account.data;
                let commit_type = if data.is_empty() {
                    CommitType::EmptyAccount
                } else {
                    CommitType::DataAccount
                };

                let data = if commit_type == CommitType::DataAccount {
                    Some(data.clone())
                } else {
                    None
                };

                // Create a commit status row for this account
                CommitStatusRow {
                    message_id: l1_message.id,
                    commit_id: 0, // Not known at creation, set later
                    pubkey: account.pubkey,
                    delegated_account_owner: account.account.owner,
                    slot: l1_message.slot,
                    ephemeral_blockhash: l1_message.blockhash,
                    undelegate,
                    lamports: account.account.lamports,
                    data,
                    commit_type,
                    created_at,
                    commit_strategy: CommitStrategy::default(),
                    commit_status: CommitStatus::Pending,
                    last_retried_at: created_at,
                    retries_count: 0,
                }
            })
            .collect()
    }
}

impl L1MessagesPersisterIface for L1MessagePersister {
    fn start_l1_messages(
        &self,
        l1_message: &[ScheduledL1Message],
    ) -> CommitPersistResult<()> {
        let commit_rows = l1_message
            .iter()
            .map(Self::create_commit_rows)
            .flatten()
            .collect::<Vec<_>>();
        // Insert all commit rows into the database
        self.commits_db
            .lock()
            .expect(POISONED_MUTEX_MSG)
            .insert_commit_status_rows(&commit_rows)?;
        Ok(())
    }

    fn start_l1_message(
        &self,
        l1_message: &ScheduledL1Message,
    ) -> CommitPersistResult<()> {
        let commit_row = Self::create_commit_rows(l1_message);
        self.commits_db
            .lock()
            .expect(POISONED_MUTEX_MSG)
            .insert_commit_status_rows(&commit_row)?;

        Ok(())
    }

    fn set_commit_id(
        &self,
        message_id: u64,
        pubkey: &Pubkey,
        commit_id: u64,
    ) -> CommitPersistResult<()> {
        self.commits_db
            .lock()
            .expect(POISONED_MUTEX_MSG)
            .set_commit_id(message_id, pubkey, commit_id)
    }

    fn set_commit_strategy(
        &self,
        commit_id: u64,
        pubkey: &Pubkey,
        value: CommitStrategy,
    ) -> CommitPersistResult<()> {
        self.commits_db
            .lock()
            .expect(POISONED_MUTEX_MSG)
            .set_commit_strategy(commit_id, pubkey, value)
    }

    fn update_status_by_message(
        &self,
        message_id: u64,
        pubkey: &Pubkey,
        status: CommitStatus,
    ) -> CommitPersistResult<()> {
        self.commits_db
            .lock()
            .expect(POISONED_MUTEX_MSG)
            .update_status_by_message(message_id, pubkey, &status)
    }

    fn update_status_by_commit(
        &self,
        commit_id: u64,
        pubkey: &Pubkey,
        status: CommitStatus,
    ) -> CommitPersistResult<()> {
        self.commits_db
            .lock()
            .expect(POISONED_MUTEX_MSG)
            .update_status_by_commit(commit_id, pubkey, &status)
    }

    fn get_commit_statuses_by_message(
        &self,
        message_id: u64,
    ) -> CommitPersistResult<Vec<CommitStatusRow>> {
        self.commits_db
            .lock()
            .expect(POISONED_MUTEX_MSG)
            .get_commit_statuses_by_id(message_id)
    }

    fn get_commit_status_by_message(
        &self,
        message_id: u64,
        pubkey: &Pubkey,
    ) -> CommitPersistResult<Option<CommitStatusRow>> {
        self.commits_db
            .lock()
            .expect(POISONED_MUTEX_MSG)
            .get_commit_status(message_id, pubkey)
    }

    fn get_signatures_by_commit(
        &self,
        commit_id: u64,
        pubkey: &Pubkey,
    ) -> CommitPersistResult<Option<MessageSignatures>> {
        self.commits_db
            .lock()
            .expect(POISONED_MUTEX_MSG)
            .get_signatures_by_commit(commit_id, pubkey)
    }

    // fn finalize_l1_message(&self, blockhash: Hash) -> CommitPersistResult<()> {
    //     self.db.lock().expect(POISONED_MUTEX_MSG).
    // }
}

#[cfg(test)]
mod tests {
    use magicblock_committor_program::ChangedAccount;
    use solana_pubkey::Pubkey;
    use solana_sdk::signature::Signature;

    use super::*;
    use crate::persist::{CommitStatusSignatures, CommitStrategy};

    #[test]
    fn test_start_changeset_and_update_status() {
        let mut persister = L1MessagePersister::try_new(":memory:").unwrap();

        // Create a test changeset
        let mut changeset = Changeset {
            slot: 100,
            ..Default::default()
        };

        let pubkey1 = Pubkey::new_unique();
        let pubkey2 = Pubkey::new_unique();
        let owner = Pubkey::new_unique();

        // Add an empty account
        changeset.add(
            pubkey1,
            ChangedAccount::Full {
                lamports: 1000,
                owner,
                data: vec![],
                bundle_id: 1,
            },
        );

        // Add a data account
        changeset.add(
            pubkey2,
            ChangedAccount::Full {
                lamports: 2000,
                owner,
                data: vec![1, 2, 3, 4, 5],
                bundle_id: 42,
            },
        );

        changeset.request_undelegation(pubkey1);

        // Start tracking the changeset
        let blockhash = Hash::new_unique();
        let reqid = persister
            .start_l1_messages(&changeset, blockhash, true)
            .unwrap();

        // Verify the rows were inserted correctly
        let rows = persister
            .commits_db
            .get_commit_statuses_by_reqid(&reqid)
            .unwrap();
        assert_eq!(rows.len(), 2);

        let empty_account_row =
            rows.iter().find(|row| row.pubkey == pubkey1).unwrap();
        assert_eq!(empty_account_row.commit_type, CommitType::EmptyAccount);
        assert!(empty_account_row.undelegate);
        assert_eq!(empty_account_row.data, None);
        assert_eq!(empty_account_row.commit_status, CommitStatus::Pending);
        assert_eq!(empty_account_row.retries_count, 0);

        let data_account_row =
            rows.iter().find(|row| row.pubkey == pubkey2).unwrap();
        assert_eq!(data_account_row.commit_type, CommitType::DataAccount);
        assert!(!data_account_row.undelegate);
        assert_eq!(data_account_row.data, Some(vec![1, 2, 3, 4, 5]));
        assert_eq!(data_account_row.commit_status, CommitStatus::Pending);

        // Update status and verify commit status and the signatures
        let process_signature = Signature::new_unique();
        let finalize_signature = Some(Signature::new_unique());
        let new_status = CommitStatus::FailedFinalize((
            1,
            CommitStrategy::Args,
            CommitStatusSignatures {
                process_signature,
                finalize_signature,
                undelegate_signature: None,
            },
        ));
        persister
            .update_status_by_message(&reqid, &pubkey1, new_status.clone())
            .unwrap();

        let updated_row = persister
            .get_commit_status_by_message(&reqid, &pubkey1)
            .unwrap()
            .unwrap();

        assert_eq!(updated_row.commit_status, new_status);

        let signatures = persister
            .get_signatures_by_commit(new_status.bundle_id().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(signatures.processed_signature, process_signature);
        assert_eq!(signatures.finalized_signature, finalize_signature);
    }
}
