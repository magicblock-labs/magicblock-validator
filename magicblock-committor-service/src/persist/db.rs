use std::{fmt, path::Path, str::FromStr};

use rusqlite::{params, Connection, Result, Transaction};
use solana_pubkey::Pubkey;
use solana_sdk::{clock::Slot, hash::Hash, signature::Signature};

use super::{
    error::CommitPersistResult,
    utils::{i64_into_u64, u64_into_i64},
    CommitStatus, CommitStatusSignatures, CommitStrategy, CommitType,
};
use crate::persist::{error::CommitPersistError, utils::now};

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
    /// The signature of the transaction on chain that executed Commit Stage
    pub commit_stage_signature: Signature,
    /// The signature of the transaction on chain that executed Finalize Stage
    /// if they were executed in 1 tx it is the same as [Self::commit_stage_signature].
    pub finalize_stage_signature: Option<Signature>,
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
    commit_stage_signature,
    finalize_stage_signature,
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
    commit_stage_signature,
    finalize_stage_signature,
    last_retried_at,
    retries_count
FROM commit_status
"#;

// -----------------
// Bundle Signature
// -----------------
// The BundleSignature table exists to store mappings from bundle_id to the signatures used
// to commit/finalize these bundles.
// The signatures are repeated in the commit_status table, however the rows in there have a
// different lifetime than the bundle signature rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleSignatureRow {
    /// The id of the bundle that was commited
    /// If an account was not part of a bundle it is treated as a single account bundle
    /// for consistency.
    /// The bundle_id is unique
    pub bundle_id: u64,
    /// The signature of the transaction that executed Commit stage
    pub commit_stage_signature: Signature,
    /// The signature of the transaction that executed Finalize stage
    /// if Commit & Finalize stages this will equal [Self::commit_stage_signature]
    pub finalize_stage_signature: Signature,
    /// Time since epoch at which the bundle signature was created
    pub created_at: u64,
}

impl BundleSignatureRow {
    pub fn new(
        bundle_id: u64,
        commit_stage_signature: Signature,
        finalize_stage_signature: Signature,
    ) -> Self {
        let created_at = now();
        Self {
            bundle_id,
            commit_stage_signature,
            finalize_stage_signature,
            created_at,
        }
    }
}

const ALL_BUNDLE_SIGNATURE_COLUMNS: &str = r#"
    bundle_id,
    commit_stage_signature,
    finalize_stage_signature,
    created_at
"#;

const SELECT_ALL_BUNDLE_SIGNATURE_COLUMNS: &str = r#"
SELECT
    bundle_id,
    commit_stage_signature,
    finalize_stage_signature,
    created_at
FROM bundle_signature
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
                commit_stage_signature = ?2,
                finalize_stage_signature = ?3
            WHERE
                pubkey = ?4 AND message_id = ?5";

        self.conn.execute(
            query,
            params![
                status.as_str(),
                status
                    .signatures()
                    .map(|s| s.commit_stage_signature.to_string()),
                status
                    .signatures()
                    .and_then(|s| s.finalize_stage_signature)
                    .map(|s| s.to_string()),
                pubkey.to_string(),
                message_id
            ],
        )?;

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
                commit_stage_signature = ?2,
                finalize_stage_signature = ?3
            WHERE
                pubkey = ?4 AND commit_id = ?5";

        self.conn.execute(
            query,
            params![
                status.as_str(),
                status
                    .signatures()
                    .map(|s| s.commit_stage_signature.to_string()),
                status
                    .signatures()
                    .and_then(|s| s.finalize_stage_signature)
                    .map(|s| s.to_string()),
                pubkey.to_string(),
                commit_id
            ],
        )?;

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

        self.conn.execute(
            query,
            params![value.as_str(), pubkey.to_string(), commit_id],
        )?;

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

        self.conn.execute(
            query,
            params![commit_id, pubkey.to_string(), message_id],
        )?;

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
                pubkey                   TEXT NOT NULL,
                commit_id                INTEGER NOT NULL,
                delegated_account_owner  TEXT NOT NULL,
                slot                     INTEGER NOT NULL,
                ephemeral_blockhash      TEXT NOT NULL,
                undelegate               INTEGER NOT NULL,
                lamports                 INTEGER NOT NULL,
                data                     BLOB,
                commit_type              TEXT NOT NULL,
                created_at               INTEGER NOT NULL,
                commit_strategy          TEXT NOT NULL,
                commit_status            TEXT NOT NULL,
                commit_stage_signature   TEXT,
                finalize_stage_signature TEXT,
                last_retried_at          INTEGER NOT NULL,
                retries_count            INTEGER NOT NULL,
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

    // -----------------
    // Bundle Signature
    // -----------------
    pub fn create_bundle_signature_table(&self) -> Result<()> {
        match self.conn.execute_batch(
            "
        BEGIN;
            CREATE TABLE IF NOT EXISTS bundle_signature (
                bundle_id INTEGER NOT NULL PRIMARY KEY,
                commit_stage_signature TEXT NOT NULL,
                finalize_stage_signature TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_bundle_signature ON bundle_signature (bundle_id);
        COMMIT;",
        ) {
            Ok(_) => Ok(()),
            Err(err) => {
                eprintln!("Error creating bundle_signature table: {}", err);
                Err(err)
            }
        }
    }

    pub fn insert_bundle_signature_row(
        &self,
        bundle_signature: &BundleSignatureRow,
    ) -> CommitPersistResult<()> {
        let query = format!("INSERT OR REPLACE INTO bundle_signature ({ALL_BUNDLE_SIGNATURE_COLUMNS})
             VALUES (?1, ?2, ?3, ?4)");

        self.conn.execute(
            &query,
            params![
                bundle_signature.bundle_id,
                bundle_signature.commit_stage_signature.to_string(),
                bundle_signature.finalize_stage_signature.to_string(),
                u64_into_i64(bundle_signature.created_at)
            ],
        )?;

        Ok(())
    }

    pub fn get_bundle_signature_by_bundle_id(
        &self,
        bundle_id: u64,
    ) -> CommitPersistResult<Option<BundleSignatureRow>> {
        let query = format!(
            "{SELECT_ALL_BUNDLE_SIGNATURE_COLUMNS} WHERE bundle_id = ?1"
        );
        let stmt = &mut self.conn.prepare(&query)?;
        let mut rows = stmt.query(params![bundle_id])?;

        if let Some(row) = rows.next()? {
            let bundle_signature_row = Self::extract_bundle_signature_row(row)?;
            Ok(Some(bundle_signature_row))
        } else {
            Ok(None)
        }
    }

    fn extract_bundle_signature_row(
        row: &rusqlite::Row,
    ) -> CommitPersistResult<BundleSignatureRow> {
        let bundle_id: u64 = {
            let bundle_id: i64 = row.get(0)?;
            i64_into_u64(bundle_id)
        };
        let commit_stage_signature = {
            let processed_signature: String = row.get(1)?;
            Signature::from_str(processed_signature.as_str())?
        };
        let finalize_stage_signature = {
            let finalized_signature: String = row.get(2)?;
            Signature::from_str(finalized_signature.as_str())?
        };
        let created_at: u64 = {
            let created_at: i64 = row.get(3)?;
            i64_into_u64(created_at)
        };

        Ok(BundleSignatureRow {
            bundle_id,
            commit_stage_signature,
            finalize_stage_signature,
            created_at,
        })
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
        let (commit_stage_signature, finalize_stage_signature) =
            match commit.commit_status.signatures() {
                Some(sigs) => (
                    Some(sigs.commit_stage_signature),
                    sigs.finalize_stage_signature,
                ),
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
            commit_stage_signature
                .as_ref()
                .map(|s| s.to_string()),
            finalize_stage_signature
                .as_ref()
                .map(|s| s.to_string()),
            u64_into_i64(commit.last_retried_at),
            commit.retries_count,
        ],
        )?;
        Ok(())
    }

    #[cfg(test)]
    #[allow(dead_code)]
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
           commit_stage_signature, finalize_stage_signature, created_at
        FROM commit_status
        WHERE commit_id = ?1 AND pubkey = ?2
        LIMIT 1";

        let mut stmt = self.conn.prepare(query)?;
        let mut rows = stmt.query(params![commit_id, pubkey.to_string()])?;

        let result = rows
            .next()?
            .map(|row| {
                let commit_stage_signature: String = row.get(0)?;
                let finalize_stage_signature: Option<String> = row.get(1)?;
                let created_at: i64 = row.get(2)?;

                Ok::<_, CommitPersistError>(MessageSignatures {
                    commit_stage_signature: Signature::from_str(
                        &commit_stage_signature,
                    )?,
                    finalize_stage_signature: finalize_stage_signature
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
        let commit_stage_signature = {
            let commit_stage_signature: Option<String> = row.get(13)?;
            commit_stage_signature
                .map(|s| Signature::from_str(s.as_str()))
                .transpose()?
        };
        let finalize_stage_signature = {
            let finalize_stage_signature: Option<String> = row.get(14)?;
            finalize_stage_signature
                .map(|s| Signature::from_str(s.as_str()))
                .transpose()?
        };
        let sigs = commit_stage_signature.map(|s| CommitStatusSignatures {
            commit_stage_signature: s,
            finalize_stage_signature,
        });
        CommitStatus::try_from((commit_status.as_str(), sigs))?
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
    use solana_sdk::{hash::Hash, signature::Signature};
    use tempfile::NamedTempFile;

    use super::*;

    // Helper to create a test database
    fn setup_test_db() -> (CommittsDb, NamedTempFile) {
        let temp_file = NamedTempFile::new().unwrap();
        let db = CommittsDb::new(temp_file.path()).unwrap();
        db.create_commit_status_table().unwrap();
        db.create_bundle_signature_table().unwrap();

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
    fn test_bundle_signature_table_creation() {
        let (db, _) = setup_test_db();

        // Verify bundle_signature table exists
        let table_exists: bool = db.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='bundle_signature')",
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
        db.insert_commit_status_rows(&[row1.clone(), row2.clone()])
            .unwrap();

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
    fn test_insert_and_retrieve_bundle_signature() {
        let (db, _file) = setup_test_db();
        let bundle_id = 123;
        let commit_sig = Signature::new_unique();
        let finalize_sig = Signature::new_unique();

        let bundle_row =
            BundleSignatureRow::new(bundle_id, commit_sig, finalize_sig);
        db.insert_bundle_signature_row(&bundle_row).unwrap();

        let retrieved = db
            .get_bundle_signature_by_bundle_id(bundle_id)
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.bundle_id, bundle_id);
        assert_eq!(retrieved.commit_stage_signature, commit_sig);
        assert_eq!(retrieved.finalize_stage_signature, finalize_sig);
    }

    #[test]
    fn test_get_nonexistent_bundle_signature() {
        let (db, _file) = setup_test_db();
        let result = db.get_bundle_signature_by_bundle_id(999).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_set_commit_id() {
        let (mut db, _file) = setup_test_db();
        let row = create_test_row(1, 0);
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
        db.update_status_by_message(1, &row.pubkey, &new_status)
            .unwrap();

        let updated = db.get_commit_status(1, &row.pubkey).unwrap().unwrap();
        assert_eq!(updated.commit_status, new_status);
    }

    #[test]
    fn test_update_status_by_commit() {
        let (mut db, _file) = setup_test_db();
        let row = create_test_row(1, 100); // Set commit_id to 100
        db.insert_commit_status_rows(&[row.clone()]).unwrap();

        let new_status = CommitStatus::Succeeded(CommitStatusSignatures {
            commit_stage_signature: Signature::new_unique(),
            finalize_stage_signature: None,
        });
        db.update_status_by_commit(100, &row.pubkey, &new_status)
            .unwrap();

        let updated = db.get_commit_status(1, &row.pubkey).unwrap().unwrap();
        assert_eq!(updated.commit_status, new_status);
    }

    #[test]
    fn test_set_commit_strategy() {
        let (mut db, _file) = setup_test_db();
        let row = create_test_row(1, 100);
        db.insert_commit_status_rows(&[row.clone()]).unwrap();

        let new_strategy = CommitStrategy::FromBuffer;
        db.set_commit_strategy(100, &row.pubkey, new_strategy)
            .unwrap();

        let updated = db.get_commit_status(1, &row.pubkey).unwrap().unwrap();
        assert_eq!(updated.commit_strategy, new_strategy);
    }

    #[test]
    fn test_get_signatures_by_commit() {
        let (mut db, _file) = setup_test_db();
        let commit_stage_signature = Signature::new_unique();
        let finalize_sig = Signature::new_unique();

        let mut row = create_test_row(1, 100);
        row.commit_status = CommitStatus::Succeeded(CommitStatusSignatures {
            commit_stage_signature,
            finalize_stage_signature: Some(finalize_sig),
        });
        db.insert_commit_status_rows(&[row.clone()]).unwrap();

        let sigs = db
            .get_signatures_by_commit(100, &row.pubkey)
            .unwrap()
            .unwrap();
        assert_eq!(sigs.commit_stage_signature, commit_stage_signature);
        assert_eq!(sigs.finalize_stage_signature, Some(finalize_sig));
    }

    #[test]
    fn test_remove_commit_statuses() {
        let (mut db, _file) = setup_test_db();
        let row1 = create_test_row(1, 0);
        let row2 = create_test_row(2, 0);
        db.insert_commit_status_rows(&[row1.clone(), row2.clone()])
            .unwrap();

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
