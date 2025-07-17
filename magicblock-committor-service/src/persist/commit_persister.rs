use std::{
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};
use std::sync::{Arc, Mutex};
use magicblock_committor_program::Changeset;
use solana_sdk::{hash::Hash, pubkey::Pubkey};
use magicblock_program::magic_scheduled_l1_message::ScheduledL1Message;
use super::{
    db::{CommitStatusRow},
    error::{CommitPersistError, CommitPersistResult},
    utils::now,
    CommitStatus, CommitType, CommittorDb,
};

const POISONED_MUTEX_MSG: &str = "Commitor Persister lock poisoned";

/// Records lifespan pf commit
pub trait CommitPersisterIface: Send + Sync + Clone {
    /// Starts persisting L1Message
    fn start_l1_messages(&self, l1_message: &ScheduledL1Message) -> CommitPersistResult<()>;
    fn start_l1_message(&self, l1_message: &ScheduledL1Message) -> CommitPersistResult<()>;
    fn update_status(&self, message_id: u64, status: CommitStatus) -> CommitPersistResult<()>;
    fn get_commit_statuses_by_id(
        &self,
        message_id: u64,
    ) -> CommitPersistResult<Vec<CommitStatusRow>>;
    fn get_commit_status(
        &self,
        message_id: u64,
        pubkey: &Pubkey,
    ) -> CommitPersistResult<Option<CommitStatusRow>>;
    // fn finalize_l1_message()
}

#[derive(Clone)]
pub struct CommitPersister {
    db: Arc<Mutex<CommittorDb>>,
}

impl CommitPersister {
    pub fn try_new<P>(db_file: P) -> CommitPersistResult<Self>
    where
        P: AsRef<Path>,
    {
        let db = CommittorDb::new(db_file)?;
        db.create_commit_status_table()?;
        db.create_bundle_signature_table()?;

        Ok(Self {
            db: Arc::new(Mutex::new(db))
        })
    }

    fn create_row(l1_message: &ScheduledL1Message) -> CommitPersistResult<CommitStatusRow> {
        let undelegate = l1_message.accounts_to_undelegate.contains(pubkey);
        let commit_type = if changed_account.data().is_empty() {
            CommitType::EmptyAccount
        } else {
            CommitType::DataAccount
        };

        let data = if commit_type == CommitType::DataAccount {
            Some(changed_account.data().to_vec())
        } else {
            None
        };

        let now = now();

        // Create a commit status row for this account
        let commit_row = CommitStatusRow {
            reqid: reqid.clone(),
            pubkey: *pubkey,
            delegated_account_owner: changed_account.owner(),
            slot: changeset.slot,
            ephemeral_blockhash,
            undelegate,
            lamports: changed_account.lamports(),
            finalize,
            data,
            commit_type,
            created_at: now,
            commit_status: CommitStatus::Pending,
            last_retried_at: now,
            retries_count: 0,
        };
    }

    pub fn update_status(
        &mut self,
        reqid: &str,
        pubkey: &Pubkey,
        status: CommitStatus,
    ) -> Result<(), CommitPersistError> {
        // NOTE: only Pending commits don't have a bundle id, but we should
        //       never update to Pending
        let Some(bundle_id) = status.bundle_id() else {
            return Err(
                CommitPersistError::CommitStatusUpdateRequiresStatusWithBundleId(
                    status.as_str().to_string(),
                ),
            );
        };

        let bundle_signature = status.signatures().map(|sigs| {
            BundleSignatureRow::new(
                bundle_id,
                sigs.process_signature,
                sigs.finalize_signature,
                sigs.undelegate_signature,
            )
        });

        self.db.update_commit_status_and_bundle_signature(
            reqid,
            pubkey,
            &status,
            bundle_signature,
        )

        // TODO(thlorenz): @@ once we see this works remove the succeeded commits
    }
}


impl CommitPersisterIface for CommitPersister {
    fn start_l1_messages(
        &self,
        l1_message: &Vec<ScheduledL1Message>,
    ) -> CommitPersistResult<()> {
        let commit_rows = l1_message.iter().map(Self::create_row).collect();
        // Insert all commit rows into the database
        self.db.lock().expect(POISONED_MUTEX_MSG).insert_commit_status_rows(&commit_rows)?;
        Ok(())
    }

    fn start_l1_message(&self, l1_message: &ScheduledL1Message) -> CommitPersistResult<()> {
        let commit_row = Self::create_row(l1_message)?;
        self.db.lock().expect(POISONED_MUTEX_MSG).insert_commit_status_rows(&[commit_row])?;

        Ok(())
    }

    fn update_status(&self, message_id: u64, status: CommitStatus) -> CommitPersistResult<()> {

    }

    fn get_commit_statuses_by_id(
        &self,
        message_id: u64,
    ) -> CommitPersistResult<Vec<CommitStatusRow>> {
        self.db.lock().expect(POISONED_MUTEX_MSG).get_commit_statuses_by_id(message_id)
    }

    fn get_commit_status(
        &self,
        message_id: u64,
        pubkey: &Pubkey,
    ) -> CommitPersistResult<Option<CommitStatusRow>> {
        self.db.lock().expect(POISONED_MUTEX_MSG).get_commit_status(message_id, pubkey)
    }
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
        let mut persister = CommitPersister::try_new(":memory:").unwrap();

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
        let rows = persister.db.get_commit_statuses_by_reqid(&reqid).unwrap();
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
            .update_status(&reqid, &pubkey1, new_status.clone())
            .unwrap();

        let updated_row = persister
            .get_commit_status(&reqid, &pubkey1)
            .unwrap()
            .unwrap();

        assert_eq!(updated_row.commit_status, new_status);

        let signatures = persister
            .get_signature(new_status.bundle_id().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(signatures.processed_signature, process_signature);
        assert_eq!(signatures.finalized_signature, finalize_signature);
    }
}
