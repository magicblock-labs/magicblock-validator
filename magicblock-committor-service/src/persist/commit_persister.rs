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

/// Blanket implementation for Option
impl<T: L1MessagesPersisterIface> L1MessagesPersisterIface for Option<T> {
    fn start_l1_messages(
        &self,
        l1_messages: &[ScheduledL1Message],
    ) -> CommitPersistResult<()> {
        match self {
            Some(persister) => persister.start_l1_messages(l1_messages),
            None => Ok(()),
        }
    }

    fn start_l1_message(
        &self,
        l1_message: &ScheduledL1Message,
    ) -> CommitPersistResult<()> {
        match self {
            Some(persister) => persister.start_l1_message(l1_message),
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
}

#[cfg(test)]
mod tests {
    use magicblock_program::magic_scheduled_l1_message::{
        CommitType, CommittedAccountV2, MagicL1Message,
    };
    use solana_sdk::{
        account::Account, hash::Hash, pubkey::Pubkey, signature::Signature,
        transaction::Transaction,
    };
    use tempfile::NamedTempFile;

    use super::*;
    use crate::persist::{db, types, CommitStatusSignatures};

    fn create_test_persister() -> (L1MessagePersister, NamedTempFile) {
        let temp_file = NamedTempFile::new().unwrap();
        let persister = L1MessagePersister::try_new(temp_file.path()).unwrap();
        (persister, temp_file)
    }

    fn create_test_message(id: u64) -> ScheduledL1Message {
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

        ScheduledL1Message {
            id,
            slot: 100,
            blockhash: Hash::new_unique(),
            action_sent_transaction: Transaction::default(),
            payer: Pubkey::new_unique(),
            l1_message: MagicL1Message::Commit(CommitType::Standalone(vec![
                CommittedAccountV2 {
                    pubkey: Pubkey::new_unique(),
                    account: account1,
                },
                CommittedAccountV2 {
                    pubkey: Pubkey::new_unique(),
                    account: account2,
                },
            ])),
        }
    }

    #[test]
    fn test_create_commit_rows() {
        let message = create_test_message(1);
        let rows = L1MessagePersister::create_commit_rows(&message);

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
    fn test_start_l1_message() {
        let (persister, _temp_file) = create_test_persister();
        let message = create_test_message(1);

        persister.start_l1_message(&message).unwrap();

        let expected_statuses =
            L1MessagePersister::create_commit_rows(&message);
        let statuses = persister.get_commit_statuses_by_message(1).unwrap();

        assert_eq!(statuses.len(), 2);
        assert_eq!(expected_statuses[0], statuses[0]);
        assert_eq!(expected_statuses[1], statuses[1]);
    }

    #[test]
    fn test_start_l1_messages() {
        let (persister, _temp_file) = create_test_persister();
        let message1 = create_test_message(1);
        let message2 = create_test_message(2);

        persister.start_l1_messages(&[message1, message2]).unwrap();

        let statuses1 = persister.get_commit_statuses_by_message(1).unwrap();
        let statuses2 = persister.get_commit_statuses_by_message(2).unwrap();
        assert_eq!(statuses1.len(), 2);
        assert_eq!(statuses2.len(), 2);
    }

    #[test]
    fn test_update_status() {
        let (persister, _temp_file) = create_test_persister();
        let message = create_test_message(1);
        persister.start_l1_message(&message).unwrap();

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
                CommitStatus::BufferAndChunkInitialized(100),
            )
            .unwrap();

        let updated = persister
            .get_commit_status_by_message(1, &pubkey)
            .unwrap()
            .unwrap();
        assert_eq!(
            updated.commit_status,
            CommitStatus::BufferAndChunkInitialized(100)
        );
    }

    #[test]
    fn test_set_commit_strategy() {
        let (persister, _temp_file) = create_test_persister();
        let message = create_test_message(1);
        persister.start_l1_message(&message).unwrap();

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
        persister.start_l1_message(&message).unwrap();

        let statuses = persister.get_commit_statuses_by_message(1).unwrap();
        let pubkey = statuses[0].pubkey;
        persister.set_commit_id(1, &pubkey, 100).unwrap();

        let process_sig = Signature::new_unique();
        let finalize_sig = Signature::new_unique();
        let status = CommitStatus::Succeeded((
            100,
            CommitStatusSignatures {
                process_signature: process_sig,
                finalize_signature: Some(finalize_sig),
            },
        ));

        persister
            .update_status_by_commit(100, &pubkey, status)
            .unwrap();

        let sigs = persister
            .get_signatures_by_commit(100, &pubkey)
            .unwrap()
            .unwrap();
        assert_eq!(sigs.processed_signature, process_sig);
        assert_eq!(sigs.finalized_signature, Some(finalize_sig));
    }

    #[test]
    fn test_empty_accounts_not_persisted() {
        let (persister, _temp_file) = create_test_persister();
        let message = ScheduledL1Message {
            l1_message: MagicL1Message::L1Actions(vec![]), // No committed accounts
            ..create_test_message(1)
        };

        persister.start_l1_message(&message).unwrap();

        let statuses = persister.get_commit_statuses_by_message(1).unwrap();
        assert_eq!(statuses.len(), 0); // No rows should be persisted
    }
}
