use std::{
    collections::HashSet,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use magicblock_committor_program::{ChangedAccount, Changeset};
use solana_sdk::{hash::Hash, pubkey::Pubkey, signature::Signature};

use super::{
    db::{BundleSignatureRow, CommitStatusRow, RegisterRetryDetails},
    error::{CommitPersistError, CommitPersistResult},
    utils::now,
    CommitStatus, CommitType, CommittorDb,
};

pub struct CommitPersister {
    db: CommittorDb,
    request_id_counter: AtomicU64,
}

impl CommitPersister {
    pub fn try_new<P>(db_file: P) -> CommitPersistResult<Self>
    where
        P: AsRef<Path>,
    {
        let db = CommittorDb::new(db_file)?;
        db.create_commit_status_table()?;
        db.create_bundle_signature_table()?;
        Ok(Self::for_db(db))
    }

    fn for_db(db: CommittorDb) -> Self {
        Self {
            db,
            request_id_counter: AtomicU64::new(1),
        }
    }

    /// Generates a unique request ID for a changeset
    fn generate_reqid(&self) -> String {
        let id = self.request_id_counter.fetch_add(1, Ordering::SeqCst);
        format!("req-{}", id)
    }

    pub fn start_changeset(
        &mut self,
        changeset: &Changeset,
        ephemeral_blockhash: Hash,
        finalize: bool,
    ) -> CommitPersistResult<String> {
        let reqid = self.generate_reqid();

        let mut commit_rows = Vec::new();

        for (pubkey, changed_account) in changeset.accounts.iter() {
            commit_rows.push(new_commit_status_row(
                &reqid,
                pubkey,
                changeset,
                changed_account,
                ephemeral_blockhash,
                finalize,
            ));
        }

        // Insert all commit rows into the database
        self.db.insert_commit_status_rows(&commit_rows)?;

        Ok(reqid)
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
                reqid,
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
    }

    pub fn update_finalize_signature(
        &mut self,
        reqid: &str,
        pubkey: &Pubkey,
        finalize_signature: &Signature,
    ) -> CommitPersistResult<()> {
        let bundle_id =
            self.db.get_bundle_id_by_reqid_and_pubkey(reqid, pubkey)?;
        self.db
            .update_finalize_signature(bundle_id, pubkey, finalize_signature)
    }

    pub fn get_commit_statuses_by_reqid(
        &self,
        reqid: &str,
    ) -> CommitPersistResult<Vec<CommitStatusRow>> {
        self.db.get_commit_statuses_by_reqid(reqid)
    }

    pub fn get_commit_status(
        &self,
        reqid: &str,
        pubkey: &Pubkey,
    ) -> CommitPersistResult<Option<CommitStatusRow>> {
        self.db.get_commit_status(reqid, pubkey)
    }

    pub fn get_reqids(&self) -> CommitPersistResult<HashSet<String>> {
        self.db.get_reqids()
    }

    pub fn remove_commit_statuses_with_reqid(
        &mut self,
        reqid: &str,
    ) -> CommitPersistResult<usize> {
        self.db.remove_commit_statuses_with_reqid(reqid)
    }

    pub fn get_signature(
        &self,
        bundle_id: u64,
    ) -> CommitPersistResult<Option<BundleSignatureRow>> {
        self.db.get_bundle_signature_by_bundle_id(bundle_id)
    }

    pub fn register_retry(
        &mut self,
        reqid: &str,
    ) -> CommitPersistResult<RegisterRetryDetails> {
        self.db.register_retry(reqid)
    }
}

fn new_commit_status_row(
    reqid: &str,
    pubkey: &Pubkey,
    changeset: &Changeset,
    changed_account: &ChangedAccount,
    ephemeral_blockhash: Hash,
    finalize: bool,
) -> CommitStatusRow {
    let undelegate = changeset.accounts_to_undelegate.contains(pubkey);
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
    CommitStatusRow {
        reqid: reqid.to_string(),
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
            .start_changeset(&changeset, blockhash, true)
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

        // Verify we can get the request id(s)
        let reqids = persister.get_reqids().unwrap();
        assert_eq!(reqids.len(), 1);
        assert!(reqids.contains(&reqid));

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

        // Verify we can get the same request id(s)
        let reqids = persister.get_reqids().unwrap();
        assert_eq!(reqids.len(), 1);
        assert!(reqids.contains(&reqid));
    }
}
