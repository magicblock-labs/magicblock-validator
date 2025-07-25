use std::{fmt, path::Path, str::FromStr};

use rusqlite::{params, Connection, OptionalExtension, Result, Transaction};
use solana_pubkey::Pubkey;
use solana_sdk::{clock::Slot, hash::Hash, signature::Signature};

use super::{
    error::CommitPersistResult,
    utils::{i64_into_u64, u64_into_i64},
    CommitStatus, CommitStatusSignatures, CommitStrategy, CommitType,
};
use crate::persist::error::CommitPersistError;

// -----------------
// CommitStatusRow
// -----------------
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitStatusRow {
    /// ID of the messages within which this account is committed
    pub message_id: u64,
    /// The on chain address of the delegated account
    pub pubkey: Pubkey,
    /// Commit ID of an account
    /// Determined and set during runtime
    pub commit_id: u64,
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
    /// Strategy defined for Commit of a particular account
    pub commit_strategy: CommitStrategy,
    /// Time since epoch at which the commit was last retried
    pub last_retried_at: u64,
    /// Number of times the commit was retried
    pub retries_count: u16,
}

#[derive(Debug)]
pub struct MessageSignatures {
    /// The signature of the transaction on chain that processed the commit
    pub processed_signature: Signature,
    /// The signature of the transaction on chain that finalized the commit
    /// if applicable
    pub finalized_signature: Option<Signature>,
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
    commit_id: {},
    delegated_account_owner: {},
    slot: {},
    ephemeral_blockhash: {},
    undelegate: {},
    lamports: {},
    data.len: {},
    commit_type: {},
    created_at: {},
    commit_status: {},
    commit_strategy: {},
    last_retried_at: {},
    retries_count: {}
}}",
            self.message_id,
            self.pubkey,
            self.commit_id,
            self.delegated_account_owner,
            self.slot,
            self.ephemeral_blockhash,
            self.undelegate,
            self.lamports,
            self.data.as_ref().map(|x| x.len()).unwrap_or_default(),
            self.commit_type.as_str(),
            self.created_at,
            self.commit_status,
            self.commit_strategy.as_str(),
            self.last_retried_at,
            self.retries_count
        )
    }
}

const ALL_COMMIT_STATUS_COLUMNS: &str = "
    message_id,
    pubkey,
    commit_id,
    delegated_account_owner,
    slot,
    ephemeral_blockhash,
    undelegate,
    lamports,
    data,
    commit_type,
    created_at,
    commit_strategy,
    commit_status,
    processed_signature,
    finalized_signature,
    last_retried_at,
    retries_count
";

const SELECT_ALL_COMMIT_STATUS_COLUMNS: &str = r#"
SELECT
    message_id,
    pubkey,
    commit_id,
    delegated_account_owner,
    slot,
    ephemeral_blockhash,
    undelegate,
    lamports,
    data,
    commit_type,
    created_at,
    commit_strategy,
    commit_status,
    processed_signature,
    finalized_signature,
    last_retried_at,
    retries_count
FROM commit_status
"#;

// -----------------
// CommittorDb
// -----------------
pub struct CommittsDb {
    conn: Connection,
}

impl CommittsDb {
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
    pub fn update_status_by_message(
        &mut self,
        message_id: u64,
        pubkey: &Pubkey,
        status: &CommitStatus,
    ) -> CommitPersistResult<()> {
        let query = "UPDATE commit_status
            SET
                commit_status = ?1,
                processed_signature = ?2,
                finalized_signature = ?3
            WHERE
                pubkey = ?4 AND message_id = ?5";

        let tx = self.conn.transaction()?;
        tx.prepare(query)?.execute(params![
            status.as_str(),
            status.signatures().map(|s| s.process_signature.to_string()),
            status
                .signatures()
                .and_then(|s| s.finalize_signature)
                .map(|s| s.to_string()),
            pubkey.to_string(),
            message_id
        ])?;
        tx.commit()?;

        Ok(())
    }

    pub fn update_status_by_commit(
        &mut self,
        commit_id: u64,
        pubkey: &Pubkey,
        status: &CommitStatus,
    ) -> CommitPersistResult<()> {
        let query = "UPDATE commit_status
            SET
                commit_status = ?1,
                processed_signature = ?2,
                finalized_signature = ?3
            WHERE
                pubkey = ?4 AND commit_id = ?5";

        let tx = self.conn.transaction()?;
        tx.prepare(query)?.execute(params![
            status.as_str(),
            status.signatures().map(|s| s.process_signature.to_string()),
            status
                .signatures()
                .and_then(|s| s.finalize_signature)
                .map(|s| s.to_string()),
            pubkey.to_string(),
            commit_id
        ])?;
        tx.commit()?;

        Ok(())
    }

    pub fn set_commit_strategy(
        &mut self,
        commit_id: u64,
        pubkey: &Pubkey,
        value: CommitStrategy,
    ) -> CommitPersistResult<()> {
        let query = "UPDATE commit_status
            SET
                commit_strategy = ?1
            WHERE
                pubkey = ?2 AND commit_id = ?3";

        let tx = self.conn.transaction()?;
        tx.prepare(query)?.execute(params![
            value.as_str(),
            pubkey.to_string(),
            commit_id
        ])?;
        tx.commit()?;

        Ok(())
    }

    pub fn set_commit_id(
        &mut self,
        message_id: u64,
        pubkey: &Pubkey,
        commit_id: u64,
    ) -> CommitPersistResult<()> {
        let query = "UPDATE commit_status
        SET
            commit_id = ?1
        WHERE
            pubkey = ?2 AND message_id = ?3";

        let tx = self.conn.transaction()?;
        tx.prepare(query)?.execute(params![
            commit_id,
            pubkey.to_string(),
            message_id
        ])?;

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
                message_id              INTEGER NOT NULL,
                pubkey                  TEXT NOT NULL,
                commit_id               INTEGER NOT NULL,
                delegated_account_owner TEXT NOT NULL,
                slot                    INTEGER NOT NULL,
                ephemeral_blockhash     TEXT NOT NULL,
                undelegate              INTEGER NOT NULL,
                lamports                INTEGER NOT NULL,
                data                    BLOB,
                commit_type             TEXT NOT NULL,
                created_at              INTEGER NOT NULL,
                commit_strategy         TEXT NOT NULL,
                commit_status           TEXT NOT NULL,
                processed_signature     TEXT,
                finalized_signature     TEXT,
                last_retried_at         INTEGER NOT NULL,
                retries_count           INTEGER NOT NULL,
                PRIMARY KEY (message_id, commit_id, pubkey)
            );
            CREATE INDEX IF NOT EXISTS idx_commits_pubkey ON commit_status (pubkey);
            CREATE INDEX IF NOT EXISTS idx_commits_message_id ON commit_status (message_id);
            CREATE INDEX IF NOT EXISTS idx_commits_commit_id ON commit_status (commit_id);
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
        if commit_rows.is_empty() {
            return Ok(());
        }

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
        let (processed_signature, finalized_signature) =
            match commit.commit_status.signatures() {
                Some(sigs) => {
                    (Some(sigs.process_signature), sigs.finalize_signature)
                }
                None => (None, None),
            };
        tx.execute(
            &format!(
                "INSERT INTO commit_status ({ALL_COMMIT_STATUS_COLUMNS}) VALUES
            (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            ),
            params![
            commit.message_id,
            commit.pubkey.to_string(),
            commit.commit_id,
            commit.delegated_account_owner.to_string(),
            u64_into_i64(commit.slot),
            commit.ephemeral_blockhash.to_string(),
            if commit.undelegate { 1 } else { 0 },
            u64_into_i64(commit.lamports),
            commit.data.as_deref(),
            commit.commit_type.as_str(),
            u64_into_i64(commit.created_at),
            commit.commit_strategy.as_str(),
            commit.commit_status.as_str(),
            processed_signature
                .as_ref()
                .map(|s| s.to_string()),
            finalized_signature
                .as_ref()
                .map(|s| s.to_string()),
            u64_into_i64(commit.last_retried_at),
            commit.retries_count,
        ],
        )?;
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

    pub fn get_signatures_by_commit(
        &self,
        commit_id: u64,
        pubkey: &Pubkey,
    ) -> CommitPersistResult<Option<MessageSignatures>> {
        let query = "
        SELECT
           processed_signature, finalized_signature, created_at
        FROM commit_status
        WHERE commit_id = ?1 AND pubkey = ?2
        LIMIT 1";

        let mut stmt = self.conn.prepare(&query)?;
        let mut rows = stmt.query(params![commit_id, pubkey.to_string()])?;

        let result = rows
            .next()?
            .map(|row| {
                let processed_signature: String = row.get(0)?;
                let finalized_signature: Option<String> = row.get(1)?;
                let created_at: i64 = row.get(2)?;

                Ok::<_, CommitPersistError>(MessageSignatures {
                    processed_signature: Signature::from_str(
                        &processed_signature,
                    )?,
                    finalized_signature: finalized_signature
                        .map(|s| Signature::from_str(&s))
                        .transpose()?,
                    created_at: i64_into_u64(created_at),
                })
            })
            .transpose()?;

        Ok(result)
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
    let commit_id = {
        let message_id: i64 = row.get(2)?;
        i64_into_u64(message_id)
    };
    let delegated_account_owner = {
        let delegated_account_owner: String = row.get(3)?;
        Pubkey::try_from(delegated_account_owner.as_str())?
    };
    let slot: Slot = {
        let slot: i64 = row.get(4)?;
        i64_into_u64(slot)
    };

    let ephemeral_blockhash = {
        let ephemeral_blockhash: String = row.get(5)?;
        Hash::from_str(ephemeral_blockhash.as_str())?
    };

    let undelegate: bool = {
        let undelegate: u8 = row.get(6)?;
        undelegate == 1
    };

    let lamports: u64 = {
        let lamports: i64 = row.get(7)?;
        i64_into_u64(lamports)
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

    let commit_strategy = {
        let commit_strategy: String = row.get(11)?;
        CommitStrategy::try_from(commit_strategy.as_str())?
    };

    let commit_status = {
        let commit_status: String = row.get(12)?;
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
        let sigs = processed_signature.map(|s| CommitStatusSignatures {
            process_signature: s,
            finalize_signature: finalized_signature,
        });
        CommitStatus::try_from((commit_status.as_str(), commit_id, sigs))?
    };

    let last_retried_at: u64 = {
        let last_retried_at: i64 = row.get(15)?;
        i64_into_u64(last_retried_at)
    };
    let retries_count: u16 = {
        let retries_count: i64 = row.get(16)?;
        retries_count.try_into().unwrap_or_default()
    };

    Ok(CommitStatusRow {
        message_id,
        pubkey,
        commit_id,
        delegated_account_owner,
        slot,
        ephemeral_blockhash,
        undelegate,
        lamports,
        data,
        commit_type,
        created_at,
        commit_strategy,
        commit_status,
        last_retried_at,
        retries_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::{signature::Signature, hash::Hash};
    use tempfile::NamedTempFile;

    // Helper to create a test database
    fn setup_test_db() -> (CommittsDb, NamedTempFile) {
        let temp_file = NamedTempFile::new().unwrap();
        let mut db = CommittsDb::new(temp_file.path()).unwrap();
        db.create_commit_status_table().unwrap();

        (db, temp_file)
    }

    // Helper to create a test CommitStatusRow
    fn create_test_row(message_id: u64, commit_id: u64) -> CommitStatusRow {
        CommitStatusRow {
            message_id,
            commit_id,
            pubkey: Pubkey::new_unique(),
            delegated_account_owner: Pubkey::new_unique(),
            slot: 100,
            ephemeral_blockhash: Hash::new_unique(),
            undelegate: false,
            lamports: 1000,
            data: Some(vec![1, 2, 3]),
            commit_type: CommitType::DataAccount,
            created_at: 1000,
            commit_status: CommitStatus::Pending,
            commit_strategy: CommitStrategy::Args,
            last_retried_at: 1000,
            retries_count: 0,
        }
    }

    #[test]
    fn test_table_creation() {
        let (db, _) = setup_test_db();

        // Verify table exists
        let table_exists: bool = db.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='commit_status')",
            [],
            |row| row.get(0),
        ).unwrap();
        assert!(table_exists);
    }

    #[test]
    fn test_insert_and_retrieve_rows() {
        let (mut db, _file) = setup_test_db();
        let row1 = create_test_row(1, 0);
        let row2 = create_test_row(1, 0); // Same message_id, different pubkey

        // Insert rows
        db.insert_commit_status_rows(&[row1.clone(), row2.clone()]).unwrap();

        // Retrieve by message_id
        let rows = db.get_commit_statuses_by_id(1).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.contains(&row1));
        assert!(rows.contains(&row2));

        // Retrieve individual row
        let retrieved = db.get_commit_status(1, &row1.pubkey).unwrap().unwrap();
        assert_eq!(retrieved, row1);
    }

    #[test]
    fn test_set_commit_id() {
        let (mut db, _file) = setup_test_db();
        let mut row = create_test_row(1, 0);
        db.insert_commit_status_rows(&[row.clone()]).unwrap();

        // Update commit_id
        db.set_commit_id(1, &row.pubkey, 100).unwrap();

        // Verify update
        let updated = db.get_commit_status(1, &row.pubkey).unwrap().unwrap();
        assert_eq!(updated.commit_id, 100);
    }

    #[test]
    fn test_update_status_by_message() {
        let (mut db, _file) = setup_test_db();
        let row = create_test_row(1, 0);
        db.insert_commit_status_rows(&[row.clone()]).unwrap();

        let new_status = CommitStatus::Pending;
        db.update_status_by_message(1, &row.pubkey, &new_status).unwrap();

        let updated = db.get_commit_status(1, &row.pubkey).unwrap().unwrap();
        assert_eq!(updated.commit_status, new_status);
    }

    #[test]
    fn test_update_status_by_commit() {
        let (mut db, _file) = setup_test_db();
        let mut row = create_test_row(1, 100); // Set commit_id to 100
        db.insert_commit_status_rows(&[row.clone()]).unwrap();

        let new_status = CommitStatus::Succeeded((
            100,
            CommitStatusSignatures {
                process_signature: Signature::new_unique(),
                finalize_signature: None,
            },
        ));
        db.update_status_by_commit(100, &row.pubkey, &new_status).unwrap();

        let updated = db.get_commit_status(1, &row.pubkey).unwrap().unwrap();
        assert_eq!(updated.commit_status, new_status);
    }

    #[test]
    fn test_set_commit_strategy() {
        let (mut db, _file) = setup_test_db();
        let mut row = create_test_row(1, 100);
        db.insert_commit_status_rows(&[row.clone()]).unwrap();

        let new_strategy = CommitStrategy::FromBuffer;
        db.set_commit_strategy(100, &row.pubkey, new_strategy).unwrap();

        let updated = db.get_commit_status(1, &row.pubkey).unwrap().unwrap();
        assert_eq!(updated.commit_strategy, new_strategy);
    }

    #[test]
    fn test_get_signatures_by_commit() {
        let (mut db, _file) = setup_test_db();
        let process_sig = Signature::new_unique();
        let finalize_sig = Signature::new_unique();

        let mut row = create_test_row(1, 100);
        row.commit_status = CommitStatus::Succeeded((
            100,
            CommitStatusSignatures {
                process_signature: process_sig,
                finalize_signature: Some(finalize_sig),
            },
        ));
        db.insert_commit_status_rows(&[row.clone()]).unwrap();

        let sigs = db.get_signatures_by_commit(100, &row.pubkey).unwrap().unwrap();
        assert_eq!(sigs.processed_signature, process_sig);
        assert_eq!(sigs.finalized_signature, Some(finalize_sig));
    }

    #[test]
    fn test_remove_commit_statuses() {
        let (mut db, _file) = setup_test_db();
        let row1 = create_test_row(1, 0);
        let row2 = create_test_row(2, 0);
        db.insert_commit_status_rows(&[row1.clone(), row2.clone()]).unwrap();

        // Remove one message
        db.remove_commit_statuses_with_id(1).unwrap();

        // Verify removal
        assert!(db.get_commit_statuses_by_id(1).unwrap().is_empty());
        assert_eq!(db.get_commit_statuses_by_id(2).unwrap().len(), 1);
    }

    #[test]
    fn test_empty_data_handling() {
        let (mut db, _file) = setup_test_db();
        let mut row = create_test_row(1, 0);
        row.data = None;
        row.commit_type = CommitType::EmptyAccount;

        db.insert_commit_status_rows(&[row.clone()]).unwrap();

        let retrieved = db.get_commit_status(1, &row.pubkey).unwrap().unwrap();
        assert!(retrieved.data.is_none());
        assert_eq!(retrieved.commit_type, CommitType::EmptyAccount);
    }

    #[test]
    fn test_undelegate_flag() {
        let (mut db, _file) = setup_test_db();
        let mut row = create_test_row(1, 0);
        row.undelegate = true;

        db.insert_commit_status_rows(&[row.clone()]).unwrap();

        let retrieved = db.get_commit_status(1, &row.pubkey).unwrap().unwrap();
        assert!(retrieved.undelegate);
    }
}