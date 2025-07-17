use std::{fmt, path::Path, str::FromStr};

use rusqlite::{params, Connection, Result, Transaction};
use solana_pubkey::Pubkey;
use solana_sdk::{clock::Slot, hash::Hash, signature::Signature};

use super::{
    error::CommitPersistResult,
    utils::{i64_into_u64, now, u64_into_i64},
    CommitStatus, CommitStatusSignatures, CommitStrategy, CommitType,
};
// -----------------
// CommitStatusRow
// -----------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitStatusRow {
    /// ID of the message
    pub message_id: u64,
    /// The on chain address of the delegated account
    pub pubkey: Pubkey,
    /// The original owner of the delegated account on chain
    pub delegated_account_owner: Pubkey,
    /// The ephemeral slot at which those changes were requested
    pub slot: Slot,
    /// The ephemeral blockhash at which those changes were requested
    pub ephemeral_blockhash: Hash,
    /// If we also undelegate the account after committing it
    pub undelegate: bool,
    /// Lamports of the account in the ephemeral
    pub lamports: u64,
    /// The account data in the ephemeral (only set if the commit is for a data account)
    pub data: Option<Vec<u8>>,
    /// The type of commit that was requested, i.e. lamports only or including data
    pub commit_type: CommitType,
    /// Time since epoch at which the commit was requested
    pub created_at: u64,
    /// The current status of the commit
    /// Includes the bundle_id which will be the same for accounts whose commits
    /// need to be applied atomically in a single transaction
    /// For single accounts a bundle_id will be generated as well for consistency
    /// For Pending commits the bundle_id is not set
    pub commit_status: CommitStatus,
    /// Time since epoch at which the commit was last retried
    pub last_retried_at: u64,
    /// Number of times the commit was retried
    pub retries_count: u16,
}

pub struct MessageSignatures {
    /// The signature of the transaction on chain that processed the commit
    pub processed_signature: Signature,
    /// The signature of the transaction on chain that finalized the commit
    /// if applicable
    pub finalized_signature: Option<Signature>,
    /// The signature of the transaction on chain that undelegated the account(s)
    /// if applicable
    pub undelegate_signature: Option<Signature>,
    /// Time since epoch at which the bundle signature was created
    pub created_at: u64,
}

impl fmt::Display for CommitStatusRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CommitStatusRow {{
    message_id: {}
    pubkey: {},
    delegated_account_owner: {},
    slot: {},
    ephemeral_blockhash: {},
    undelegate: {},
    lamports: {},
    data.len: {},
    commit_type: {},
    created_at: {},
    commit_status: {},
    last_retried_at: {},
    retries_count: {}
}}",
            self.message_id,
            self.pubkey,
            self.delegated_account_owner,
            self.slot,
            self.ephemeral_blockhash,
            self.undelegate,
            self.lamports,
            self.data.as_ref().map(|x| x.len()).unwrap_or_default(),
            self.commit_type.as_str(),
            self.created_at,
            self.commit_status,
            self.last_retried_at,
            self.retries_count
        )
    }
}

const ALL_COMMIT_STATUS_COLUMNS: &str = "
    message_id,
    pubkey,
    delegated_account_owner,
    slot,
    ephemeral_blockhash,
    undelegate,
    lamports,
    finalize,
    bundle_id,
    data,
    commit_type,
    created_at,
    commit_status,
    commit_strategy,
    processed_signature,
    finalized_signature,
    undelegated_signature,
    last_retried_at,
    retries_count
";


const SELECT_ALL_COMMIT_STATUS_COLUMNS: &str = const {
    concat!("SELECT", ALL_COMMIT_STATUS_COLUMNS, "FROM commit_status")
};

// -----------------
// CommittorDb
// -----------------
pub struct CommittorDb {
    conn: Connection,
}

impl CommittorDb {
    pub fn new<P>(db_file: P) -> Result<Self>
    where
        P: AsRef<Path>,
    {
        let conn = Connection::open(db_file)?;
        Ok(Self { conn })
    }

    pub fn path(&self) -> Option<&str> {
        self.conn.path()
    }

    // -----------------
    // Methods affecting both tables
    // -----------------
    pub fn update_commit_status(
        &mut self,
        message_id: u64,
        pubkey: &Pubkey,
        status: &CommitStatus,
    ) -> CommitPersistResult<()> {
        let tx = self.conn.transaction()?;
        Self::update_commit_status_impl(&tx, message_id, pubkey, status)?;
        tx.commit()?;

        Ok(())
    }

    // -----------------
    // Commit Status
    // -----------------
    pub fn create_commit_status_table(&self) -> Result<()> {
        match self.conn.execute_batch(
                "
        BEGIN;
            CREATE TABLE IF NOT EXISTS commit_status (
                message_id               INTEGER NOT NULL,
                pubkey                  TEXT NOT NULL,
                delegated_account_owner TEXT NOT NULL,
                slot                    INTEGER NOT NULL,
                ephemeral_blockhash     TEXT NOT NULL,
                undelegate              INTEGER NOT NULL,
                lamports                INTEGER NOT NULL,
                data                    BLOB,
                commit_type             TEXT NOT NULL,
                created_at              INTEGER NOT NULL,
                commit_status           TEXT NOT NULL,
                commit_strategy         TEXT NOT NULL,
                processed_signature     TEXT,
                finalized_signature     TEXT,
                undelegated_signature   TEXT,
                last_retried_at         INTEGER NOT NULL,
                retries_count           INTEGER NOT NULL,
                PRIMARY KEY (message_id, pubkey)
            );
            CREATE INDEX IF NOT EXISTS idx_commits_pubkey ON commit_status (pubkey);
            CREATE INDEX IF NOT EXISTS idx_commits_message_id ON commit_status (message_id);
        COMMIT;",
        ) {
            Ok(_) => Ok(()),
            Err(err) => {
                eprintln!("Error creating commit_status table: {}", err);
                Err(err)
            }
        }
    }

    pub fn insert_commit_status_rows(
        &mut self,
        commit_rows: &[CommitStatusRow],
    ) -> CommitPersistResult<()> {
        let tx = self.conn.transaction()?;
        for commit in commit_rows {
            Self::insert_commit_status_row(&tx, commit)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn insert_commit_status_row(
        tx: &Transaction<'_>,
        commit: &CommitStatusRow,
    ) -> CommitPersistResult<()> {
        let (processed_signature, finalized_signature, undelegated_signature) =
            match commit.commit_status.signatures() {
                Some(sigs) => (
                    Some(sigs.process_signature),
                    sigs.finalize_signature,
                    sigs.undelegate_signature,
                ),
                None => (None, None, None),
            };
        tx.execute(
            &format!(
                "INSERT INTO commit_status ({ALL_COMMIT_STATUS_COLUMNS}) VALUES
                (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            ),
            params![
                commit.message_id,
                commit.pubkey.to_string(),
                commit.delegated_account_owner.to_string(),
                u64_into_i64(commit.slot),
                commit.ephemeral_blockhash.to_string(),
                if commit.undelegate { 1 } else { 0 },
                u64_into_i64(commit.lamports),
                commit.commit_status.bundle_id().map(u64_into_i64),
                commit.data.as_deref(),
                commit.commit_type.as_str(),
                u64_into_i64(commit.created_at),
                commit.commit_status.as_str(),
                commit.commit_status.commit_strategy().as_str(),
                processed_signature
                    .as_ref()
                    .map(|s| s.to_string()),
                finalized_signature
                    .as_ref()
                    .map(|s| s.to_string()),
                undelegated_signature
                    .as_ref()
                    .map(|s| s.to_string()),
                u64_into_i64(commit.last_retried_at),
                commit.retries_count,
            ],
        )?;
        Ok(())
    }

    fn update_commit_status_impl(
        tx: &Transaction<'_>,
        message_id: u64,
        pubkey: &Pubkey,
        status: &CommitStatus,
    ) -> CommitPersistResult<()> {
        let query = "UPDATE commit_status
            SET
                commit_status = ?1,
                bundle_id = ?2,
                commit_strategy = ?3,
                processed_signature = ?4,
                finalized_signature = ?5,
                undelegated_signature = ?6
            WHERE
                pubkey = ?7 AND message_id = ?8";
        let stmt = &mut tx.prepare(query)?;
        stmt.execute(params![
            status.as_str(),
            status.bundle_id(),
            status.commit_strategy().as_str(),
            status.signatures().map(|s| s.process_signature.to_string()),
            status
                .signatures()
                .and_then(|s| s.finalize_signature)
                .map(|s| s.to_string()),
            status
                .signatures()
                .and_then(|s| s.undelegate_signature)
                .map(|s| s.to_string()),
            pubkey.to_string(),
            message_id
        ])?;
        Ok(())
    }

    #[cfg(test)]
    fn get_commit_statuses_by_pubkey(
        &self,
        pubkey: &Pubkey,
    ) -> CommitPersistResult<Vec<CommitStatusRow>> {
        let query =
            format!("{SELECT_ALL_COMMIT_STATUS_COLUMNS} WHERE pubkey = ?1");
        let stmt = &mut self.conn.prepare(&query)?;
        let mut rows = stmt.query(params![pubkey.to_string()])?;

        extract_committor_rows(&mut rows)
    }

    pub(crate) fn get_commit_statuses_by_id(
        &self,
        message_id: u64,
    ) -> CommitPersistResult<Vec<CommitStatusRow>> {
        let query =
            format!("{SELECT_ALL_COMMIT_STATUS_COLUMNS} WHERE message_id = ?1");
        let stmt = &mut self.conn.prepare(&query)?;
        let mut rows = stmt.query(params![message_id])?;

        extract_committor_rows(&mut rows)
    }

    pub(crate) fn get_commit_status(
        &self,
        message_id: u64,
        pubkey: &Pubkey,
    ) -> CommitPersistResult<Option<CommitStatusRow>> {
        let query = format!(
            "{SELECT_ALL_COMMIT_STATUS_COLUMNS} WHERE message_id = ?1 AND pubkey = ?2"
        );
        let stmt = &mut self.conn.prepare(&query)?;
        let mut rows = stmt.query(params![message_id, pubkey.to_string()])?;

        extract_committor_rows(&mut rows).map(|mut rows| rows.pop())
    }

    pub fn remove_commit_statuses_with_id(
        &self,
        message_id: u64,
    ) -> CommitPersistResult<()> {
        let query = "DELETE FROM commit_status WHERE message_id = ?1";
        let stmt = &mut self.conn.prepare(query)?;
        stmt.execute(params![message_id])?;
        Ok(())
    }

    pub fn get_signatures_by_id(
        &self,
        message_id: u64
    ) -> CommitPersistResult<MessageSignatures> {
        todo!()
    }
}

// -----------------
// Commit Status Helpers
// -----------------
fn extract_committor_rows(
    rows: &mut rusqlite::Rows,
) -> CommitPersistResult<Vec<CommitStatusRow>> {
    let mut commits = Vec::new();
    while let Some(row) = rows.next()? {
        let commit_row = extract_committor_row(row)?;
        commits.push(commit_row);
    }
    Ok(commits)
}

fn extract_committor_row(
    row: &rusqlite::Row,
) -> CommitPersistResult<CommitStatusRow> {
    let message_id: u64 = {
        let message_id: i64 = row.get(0)?;
        i64_into_u64(message_id)
    };

    let pubkey = {
        let pubkey: String = row.get(1)?;
        Pubkey::try_from(pubkey.as_str())?
    };
    let delegated_account_owner = {
        let delegated_account_owner: String = row.get(2)?;
        Pubkey::try_from(delegated_account_owner.as_str())?
    };
    let slot: Slot = {
        let slot: i64 = row.get(3)?;
        i64_into_u64(slot)
    };

    let ephemeral_blockhash = {
        let ephemeral_blockhash: String = row.get(4)?;
        Hash::from_str(ephemeral_blockhash.as_str())?
    };

    let undelegate: bool = {
        let undelegate: u8 = row.get(5)?;
        undelegate == 1
    };

    let lamports: u64 = {
        let lamports: i64 = row.get(6)?;
        i64_into_u64(lamports)
    };

    let bundle_id: Option<u64> = {
        let bundle_id: Option<i64> = row.get(7)?;
        bundle_id.map(i64_into_u64)
    };

    let data: Option<Vec<u8>> = row.get(8)?;

    let commit_type = {
        let commit_type: String = row.get(9)?;
        CommitType::try_from(commit_type.as_str())?
    };
    let created_at: u64 = {
        let created_at: i64 = row.get(10)?;
        i64_into_u64(created_at)
    };
    let commit_status = {
        let commit_status: String = row.get(11)?;
        let commit_strategy = {
            let commit_strategy: String = row.get(12)?;
            CommitStrategy::from(commit_strategy.as_str())
        };
        let processed_signature = {
            let processed_signature: Option<String> = row.get(13)?;
            processed_signature
                .map(|s| Signature::from_str(s.as_str()))
                .transpose()?
        };
        let finalized_signature = {
            let finalized_signature: Option<String> = row.get(14)?;
            finalized_signature
                .map(|s| Signature::from_str(s.as_str()))
                .transpose()?
        };
        let undelegated_signature = {
            let undelegated_signature: Option<String> = row.get(15)?;
            undelegated_signature
                .map(|s| Signature::from_str(s.as_str()))
                .transpose()?
        };
        let sigs = processed_signature.map(|s| CommitStatusSignatures {
            process_signature: s,
            finalize_signature: finalized_signature,
            undelegate_signature: undelegated_signature,
        });
        CommitStatus::try_from((
            commit_status.as_str(),
            bundle_id,
            commit_strategy,
            sigs,
        ))?
    };

    let last_retried_at: u64 = {
        let last_retried_at: i64 = row.get(16)?;
        i64_into_u64(last_retried_at)
    };
    let retries_count: u16 = {
        let retries_count: i64 = row.get(17)?;
        retries_count.try_into().unwrap_or_default()
    };

    Ok(CommitStatusRow {
        message_id,
        pubkey,
        delegated_account_owner,
        slot,
        ephemeral_blockhash,
        undelegate,
        lamports,
        data,
        commit_type,
        created_at,
        commit_status,
        last_retried_at,
        retries_count,
    })
}


#[cfg(test)]
mod test {
    use super::*;

    fn setup_db() -> CommittorDb {
        let db = CommittorDb::new(":memory:").unwrap();
        db.create_commit_status_table().unwrap();
        db
    }

    // -----------------
    // Commit Status
    // -----------------
    fn create_commit_status_row(message_id: u64) -> CommitStatusRow {
        CommitStatusRow {
            message_id,
            pubkey: Pubkey::new_unique(),
            delegated_account_owner: Pubkey::new_unique(),
            slot: 100,
            ephemeral_blockhash: Hash::new_unique(),
            undelegate: false,
            lamports: 100,
            data: None,
            commit_type: CommitType::EmptyAccount,
            created_at: 1000,
            commit_status: CommitStatus::Pending,
            last_retried_at: 1000,
            retries_count: 0,
        }
    }

    #[test]
    fn test_round_trip_commit_status_rows() {
        let one_unbundled_commit_row_no_data = CommitStatusRow {
            message_id: 123,
            pubkey: Pubkey::new_unique(),
            delegated_account_owner: Pubkey::new_unique(),
            slot: 100,
            ephemeral_blockhash: Hash::new_unique(),
            undelegate: false,
            lamports: 100,
            data: None,
            commit_type: CommitType::EmptyAccount,
            created_at: 1000,
            commit_status: CommitStatus::Pending,
            last_retried_at: 1000,
            retries_count: 0,
        };

        let two_bundled_commit_row_with_data = CommitStatusRow {
            message_id: 123,
            pubkey: Pubkey::new_unique(),
            delegated_account_owner: Pubkey::new_unique(),
            slot: 100,
            ephemeral_blockhash: Hash::new_unique(),
            undelegate: false,
            lamports: 2000,
            data: Some(vec![1, 2, 3]),
            commit_type: CommitType::DataAccount,
            created_at: 1000,
            commit_status: CommitStatus::FailedProcess((
                2,
                CommitStrategy::Args,
                None,
            )),
            last_retried_at: 1000,
            retries_count: 0,
        };

        let mut db = setup_db();
        db.insert_commit_status_rows(&[
            one_unbundled_commit_row_no_data.clone(),
            two_bundled_commit_row_with_data.clone(),
        ])
        .unwrap();

        let one = db
            .get_commit_statuses_by_pubkey(
                &one_unbundled_commit_row_no_data.pubkey,
            )
            .unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0], one_unbundled_commit_row_no_data);

        let two = db
            .get_commit_statuses_by_pubkey(
                &two_bundled_commit_row_with_data.pubkey,
            )
            .unwrap();
        assert_eq!(two.len(), 1);
        assert_eq!(two[0], two_bundled_commit_row_with_data);

        let by_message_id = db
            .get_commit_statuses_by_id(
                one_unbundled_commit_row_no_data.message_id,
            )
            .unwrap();
        assert_eq!(by_message_id.len(), 2);
        assert_eq!(
            by_message_id,
            [
                one_unbundled_commit_row_no_data,
                two_bundled_commit_row_with_data
            ]
        );
    }

    #[test]
    fn test_commits_with_message_id() {
        let mut db = setup_db();
        const MESSAGE_ID_ONE: u64 = 123;
        const MESSAGE_ID_TWO: u64 = 456;

        let commit_row_one = create_commit_status_row(MESSAGE_ID_ONE);
        let commit_row_one_other = create_commit_status_row(MESSAGE_ID_ONE);
        let commit_row_two = create_commit_status_row(MESSAGE_ID_TWO);
        db.insert_commit_status_rows(&[
            commit_row_one.clone(),
            commit_row_one_other.clone(),
            commit_row_two.clone(),
        ])
        .unwrap();

        let commits_one = db.get_commit_statuses_by_id(MESSAGE_ID_ONE).unwrap();
        assert_eq!(commits_one.len(), 2);
        assert_eq!(commits_one[0], commit_row_one);
        assert_eq!(commits_one[1], commit_row_one_other);

        let commits_two = db.get_commit_statuses_by_id(MESSAGE_ID_TWO).unwrap();
        assert_eq!(commits_two.len(), 1);
        assert_eq!(commits_two[0], commit_row_two);

        // Remove commits with MESSAGE_ID_ONE
        db.remove_commit_statuses_with_id(MESSAGE_ID_ONE).unwrap();
        let commits_one_after_removal =
            db.get_commit_statuses_by_id(MESSAGE_ID_ONE).unwrap();
        assert_eq!(commits_one_after_removal.len(), 0);

        let commits_two_after_removal =
            db.get_commit_statuses_by_id(MESSAGE_ID_TWO).unwrap();
        assert_eq!(commits_two_after_removal.len(), 1);
    }

    #[test]
    fn test_update_commit_status() {
        let mut db = setup_db();
        const MESSAGE_ID: u64 = 123;

        let failing_commit_row = create_commit_status_row(MESSAGE_ID);
        let success_commit_row = create_commit_status_row(MESSAGE_ID);
        db.insert_commit_status_rows(&[
            failing_commit_row.clone(),
            success_commit_row.clone(),
        ])
        .unwrap();

        // Update the statuses
        let new_failing_status =
            CommitStatus::FailedProcess((22, CommitStrategy::FromBuffer, None));
        db.update_commit_status(
            failing_commit_row.message_id,
            &failing_commit_row.pubkey,
            &new_failing_status,
            None,
        )
        .unwrap();
        let sigs = CommitStatusSignatures {
            process_signature: Signature::new_unique(),
            finalize_signature: None,
            undelegate_signature: None,
        };
        let new_success_status =
            CommitStatus::Succeeded((33, CommitStrategy::Args, sigs));
        db.update_commit_status(
            success_commit_row.message_id,
            &success_commit_row.pubkey,
            &new_success_status,
        )
        .unwrap();

        // Verify the statuses were updated
        let failed_commit_row = db
            .get_commit_status(MESSAGE_ID, &failing_commit_row.pubkey)
            .unwrap()
            .unwrap();
        assert_eq!(failed_commit_row.commit_status, new_failing_status);

        let succeeded_commit_row = db
            .get_commit_status(MESSAGE_ID, &success_commit_row.pubkey)
            .unwrap()
            .unwrap();
        assert_eq!(succeeded_commit_row.commit_status, new_success_status);
        let signature_row =
            db.get_signatures_by_id(33).unwrap().unwrap();
        assert_eq!(
            signature_row.processed_signature,
            success_signatures.processed_signature,
        );
        assert_eq!(signature_row.finalized_signature, None);
    }
}
