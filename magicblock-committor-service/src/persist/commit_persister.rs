use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use magicblock_program::magic_scheduled_base_intent::ScheduledBaseIntent;
use solana_pubkey::Pubkey;

use super::{
    db::CommitStatusRow, error::CommitPersistResult, utils::now, CommitStatus,
    CommitStrategy, CommitType, CommittsDb, MessageSignatures,
};
use crate::{
    intent_executor::ExecutionOutput, persist::db::BundleSignatureRow,
};

const POISONED_MUTEX_MSG: &str = "Commitor Persister lock poisoned";

/// Records lifespan pf BaseIntent
pub trait IntentPersister: Send + Sync + Clone + 'static {
    /// Starts persisting BaseIntents
    fn start_base_intents(
        &self,
        base_intent: &[ScheduledBaseIntent],
    ) -> CommitPersistResult<()>;
    /// Starts persisting BaseIntent
    fn start_base_intent(
        &self,
        base_intent: &ScheduledBaseIntent,
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
    fn get_bundle_signatures(
        &self,
        message_id: u64,
    ) -> CommitPersistResult<Option<BundleSignatureRow>>;
    fn finalize_base_intent(
        &self,
        message_id: u64,
        execution_output: ExecutionOutput,
    ) -> CommitPersistResult<()>;
}

#[derive(Clone)]
pub struct IntentPersisterImpl {
    // DB that tracks lifespan of Commit intents
    commits_db: Arc<Mutex<CommittsDb>>,
    // TODO(edwin): add something like
    // actions_db: Arc<Mutex<ActionsDb>>
}

impl IntentPersisterImpl {
    pub fn try_new<P>(db_file: P) -> CommitPersistResult<Self>
    where
        P: AsRef<Path>,
    {
        let db = CommittsDb::new(db_file)?;
        db.create_commit_status_table()?;
        db.create_bundle_signature_table()?;

        Ok(Self {
            commits_db: Arc::new(Mutex::new(db)),
        })
    }

    pub fn create_commit_rows(
        base_intent: &ScheduledBaseIntent,
    ) -> Vec<CommitStatusRow> {
        let Some(committed_accounts) = base_intent.get_committed_accounts()
        else {
            // We don't persist standalone actions
            return vec![];
        };

        let undelegate = base_intent.is_undelegate();
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
                    message_id: base_intent.id,
                    commit_id: 0, // Not known at creation, set later
                    pubkey: account.pubkey,
                    delegated_account_owner: account.account.owner,
                    slot: base_intent.slot,
                    ephemeral_blockhash: base_intent.blockhash,
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

impl IntentPersister for IntentPersisterImpl {
    fn start_base_intents(
        &self,
        base_intents: &[ScheduledBaseIntent],
    ) -> CommitPersistResult<()> {
        let commit_rows = base_intents
            .iter()
            .flat_map(Self::create_commit_rows)
            .collect::<Vec<_>>();
        // Insert all commit rows into the database
        self.commits_db
            .lock()
            .expect(POISONED_MUTEX_MSG)
            .insert_commit_status_rows(&commit_rows)?;
        Ok(())
    }

    fn start_base_intent(
        &self,
        base_intents: &ScheduledBaseIntent,
    ) -> CommitPersistResult<()> {
        let commit_row = Self::create_commit_rows(base_intents);
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

    fn get_bundle_signatures(
        &self,
        message_id: u64,
    ) -> CommitPersistResult<Option<BundleSignatureRow>> {
        self.commits_db
            .lock()
            .expect(POISONED_MUTEX_MSG)
            .get_bundle_signature_by_bundle_id(message_id)
    }

    fn finalize_base_intent(
        &self,
        message_id: u64,
        execution_output: ExecutionOutput,
    ) -> CommitPersistResult<()> {
        let (commit_signature, finalize_signature) = match execution_output {
            ExecutionOutput::SingleStage(signature) => (signature, signature),
            ExecutionOutput::TwoStage {
                commit_signature,
                finalize_signature,
            } => (commit_signature, finalize_signature),
        };

        let bundle_signature_row = BundleSignatureRow::new(
            message_id,
            commit_signature,
            finalize_signature,
        );
        let commits_db = self.commits_db.lock().expect(POISONED_MUTEX_MSG);
        commits_db.insert_bundle_signature_row(&bundle_signature_row)?;
        Ok(())
    }
}

/// Blanket implementation for Option
impl<T: IntentPersister> IntentPersister for Option<T> {
    fn start_base_intents(
        &self,
        base_intents: &[ScheduledBaseIntent],
    ) -> CommitPersistResult<()> {
        match self {
            Some(persister) => persister.start_base_intents(base_intents),
            None => Ok(()),
        }
    }

    fn start_base_intent(
        &self,
        base_intents: &ScheduledBaseIntent,
    ) -> CommitPersistResult<()> {
        match self {
            Some(persister) => persister.start_base_intent(base_intents),
            None => Ok(()),
        }
    }

    fn set_commit_id(
        &self,
        message_id: u64,
        pubkey: &Pubkey,
        commit_id: u64,
    ) -> CommitPersistResult<()> {
        match self {
            Some(persister) => {
                persister.set_commit_id(message_id, pubkey, commit_id)
            }
            None => Ok(()),
        }
    }

    fn set_commit_strategy(
        &self,
        commit_id: u64,
        pubkey: &Pubkey,
        value: CommitStrategy,
    ) -> CommitPersistResult<()> {
        match self {
            Some(persister) => {
                persister.set_commit_strategy(commit_id, pubkey, value)
            }
            None => Ok(()),
        }
    }

    fn update_status_by_message(
        &self,
        message_id: u64,
        pubkey: &Pubkey,
        status: CommitStatus,
    ) -> CommitPersistResult<()> {
        match self {
            Some(persister) => {
                persister.update_status_by_message(message_id, pubkey, status)
            }
            None => Ok(()),
        }
    }

    fn update_status_by_commit(
        &self,
        commit_id: u64,
        pubkey: &Pubkey,
        status: CommitStatus,
    ) -> CommitPersistResult<()> {
        match self {
            Some(persister) => {
                persister.update_status_by_commit(commit_id, pubkey, status)
            }
            None => Ok(()),
        }
    }

    fn get_commit_statuses_by_message(
        &self,
        message_id: u64,
    ) -> CommitPersistResult<Vec<CommitStatusRow>> {
        match self {
            Some(persister) => {
                persister.get_commit_statuses_by_message(message_id)
            }
            None => Ok(Vec::new()),
        }
    }

    fn get_commit_status_by_message(
        &self,
        message_id: u64,
        pubkey: &Pubkey,
    ) -> CommitPersistResult<Option<CommitStatusRow>> {
        match self {
            Some(persister) => {
                persister.get_commit_status_by_message(message_id, pubkey)
            }
            None => Ok(None),
        }
    }

    fn get_signatures_by_commit(
        &self,
        commit_id: u64,
        pubkey: &Pubkey,
    ) -> CommitPersistResult<Option<MessageSignatures>> {
        match self {
            Some(persister) => {
                persister.get_signatures_by_commit(commit_id, pubkey)
            }
            None => Ok(None),
        }
    }

    fn get_bundle_signatures(
        &self,
        message_id: u64,
    ) -> CommitPersistResult<Option<BundleSignatureRow>> {
        match self {
            Some(persister) => persister.get_bundle_signatures(message_id),
            None => Ok(None),
        }
    }

    fn finalize_base_intent(
        &self,
        message_id: u64,
        execution_output: ExecutionOutput,
    ) -> CommitPersistResult<()> {
        match self {
            Some(persister) => {
                persister.finalize_base_intent(message_id, execution_output)
            }
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use magicblock_program::magic_scheduled_base_intent::{
        CommitType, CommittedAccount, MagicBaseIntent,
    };
    use solana_account::Account;
    use solana_hash::Hash;
    use solana_pubkey::Pubkey;
    use solana_signature::Signature;
    use solana_transaction::Transaction;
    use tempfile::NamedTempFile;

    use super::*;
    use crate::persist::{types, CommitStatusSignatures};

    fn create_test_persister() -> (IntentPersisterImpl, NamedTempFile) {
        let temp_file = NamedTempFile::new().unwrap();
        let persister = IntentPersisterImpl::try_new(temp_file.path()).unwrap();
        (persister, temp_file)
    }

    fn create_test_message(id: u64) -> ScheduledBaseIntent {
        let account1 = Account {
            lamports: 1000,
            owner: Pubkey::new_unique(),
            data: vec![],
            executable: false,
            rent_epoch: 0,
        };
        let account2 = Account {
            lamports: 2000,
            owner: Pubkey::new_unique(),
            data: vec![1, 2, 3],
            executable: false,
            rent_epoch: 0,
        };

        ScheduledBaseIntent {
            id,
            slot: 100,
            blockhash: Hash::new_unique(),
            action_sent_transaction: Transaction::default(),
            payer: Pubkey::new_unique(),
            base_intent: MagicBaseIntent::Commit(CommitType::Standalone(vec![
                CommittedAccount {
                    pubkey: Pubkey::new_unique(),
                    account: account1,
                },
                CommittedAccount {
                    pubkey: Pubkey::new_unique(),
                    account: account2,
                },
            ])),
        }
    }

    #[test]
    fn test_create_commit_rows() {
        let message = create_test_message(1);
        let rows = IntentPersisterImpl::create_commit_rows(&message);

        assert_eq!(rows.len(), 2);

        let empty_account = rows.iter().find(|r| r.data.is_none()).unwrap();
        assert_eq!(empty_account.commit_type, types::CommitType::EmptyAccount);
        assert_eq!(empty_account.lamports, 1000);

        let data_account = rows.iter().find(|r| r.data.is_some()).unwrap();
        assert_eq!(data_account.commit_type, types::CommitType::DataAccount);
        assert_eq!(data_account.lamports, 2000);
        assert_eq!(data_account.data, Some(vec![1, 2, 3]));
    }

    #[test]
    fn test_start_base_message() {
        let (persister, _temp_file) = create_test_persister();
        let message = create_test_message(1);

        persister.start_base_intent(&message).unwrap();

        let expected_statuses =
            IntentPersisterImpl::create_commit_rows(&message);
        let statuses = persister.get_commit_statuses_by_message(1).unwrap();

        assert_eq!(statuses.len(), 2);
        assert_eq!(expected_statuses[0], statuses[0]);
        assert_eq!(expected_statuses[1], statuses[1]);
    }

    #[test]
    fn test_start_base_messages() {
        let (persister, _temp_file) = create_test_persister();
        let message1 = create_test_message(1);
        let message2 = create_test_message(2);

        persister.start_base_intents(&[message1, message2]).unwrap();

        let statuses1 = persister.get_commit_statuses_by_message(1).unwrap();
        let statuses2 = persister.get_commit_statuses_by_message(2).unwrap();
        assert_eq!(statuses1.len(), 2);
        assert_eq!(statuses2.len(), 2);
    }

    #[test]
    fn test_update_status() {
        let (persister, _temp_file) = create_test_persister();
        let message = create_test_message(1);
        persister.start_base_intent(&message).unwrap();

        let pubkey = message.get_committed_pubkeys().unwrap()[0];

        // Update by message
        persister
            .update_status_by_message(1, &pubkey, CommitStatus::Pending)
            .unwrap();

        let updated = persister
            .get_commit_status_by_message(1, &pubkey)
            .unwrap()
            .unwrap();
        assert_eq!(updated.commit_status, CommitStatus::Pending);

        // Set commit ID and update by commit
        persister.set_commit_id(1, &pubkey, 100).unwrap();
        persister
            .update_status_by_commit(
                100,
                &pubkey,
                CommitStatus::BufferAndChunkInitialized,
            )
            .unwrap();

        let updated = persister
            .get_commit_status_by_message(1, &pubkey)
            .unwrap()
            .unwrap();
        assert_eq!(
            updated.commit_status,
            CommitStatus::BufferAndChunkInitialized
        );
    }

    #[test]
    fn test_set_commit_strategy() {
        let (persister, _temp_file) = create_test_persister();
        let message = create_test_message(1);
        persister.start_base_intent(&message).unwrap();

        let pubkey = message.get_committed_pubkeys().unwrap()[0];
        persister.set_commit_id(1, &pubkey, 100).unwrap();

        persister
            .set_commit_strategy(100, &pubkey, CommitStrategy::Args)
            .unwrap();

        let updated = persister
            .get_commit_status_by_message(1, &pubkey)
            .unwrap()
            .unwrap();
        assert_eq!(updated.commit_strategy, CommitStrategy::Args);
    }

    #[test]
    fn test_get_signatures() {
        let (persister, _temp_file) = create_test_persister();
        let message = create_test_message(1);
        persister.start_base_intent(&message).unwrap();

        let statuses = persister.get_commit_statuses_by_message(1).unwrap();
        let pubkey = statuses[0].pubkey;
        persister.set_commit_id(1, &pubkey, 100).unwrap();

        let process_sig = Signature::new_unique();
        let finalize_sig = Signature::new_unique();
        let status = CommitStatus::Succeeded(CommitStatusSignatures {
            commit_stage_signature: process_sig,
            finalize_stage_signature: Some(finalize_sig),
        });

        persister
            .update_status_by_commit(100, &pubkey, status)
            .unwrap();

        let sigs = persister
            .get_signatures_by_commit(100, &pubkey)
            .unwrap()
            .unwrap();
        assert_eq!(sigs.commit_stage_signature, process_sig);
        assert_eq!(sigs.finalize_stage_signature, Some(finalize_sig));
    }

    #[test]
    fn test_finalize_base_intent() {
        let (persister, _temp_file) = create_test_persister();
        let message_id = 1;

        let commit_sig = Signature::new_unique();
        let finalize_sig = Signature::new_unique();
        let execution_output = ExecutionOutput::TwoStage {
            commit_signature: commit_sig,
            finalize_signature: finalize_sig,
        };

        persister
            .finalize_base_intent(message_id, execution_output)
            .unwrap();

        let bundle_sig = persister
            .get_bundle_signatures(message_id)
            .unwrap()
            .unwrap();
        assert_eq!(bundle_sig.bundle_id, message_id);
        assert_eq!(bundle_sig.commit_stage_signature, commit_sig);
        assert_eq!(bundle_sig.finalize_stage_signature, finalize_sig);
    }

    #[test]
    fn test_empty_accounts_not_persisted() {
        let (persister, _temp_file) = create_test_persister();
        let message = ScheduledBaseIntent {
            base_intent: MagicBaseIntent::BaseActions(vec![]), // No committed accounts
            ..create_test_message(1)
        };

        persister.start_base_intent(&message).unwrap();

        let statuses = persister.get_commit_statuses_by_message(1).unwrap();
        assert_eq!(statuses.len(), 0); // No rows should be persisted
    }
}
