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
    /// Request ID that is common for some accounts
    pub reqid: String,
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
    /// If `true` the account commit is finalized after it was processed
    pub finalize: bool,
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

impl fmt::Display for CommitStatusRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CommitStatusRow {{
    reqid: {}
    pubkey: {},
    delegated_account_owner: {},
    slot: {},
    ephemeral_blockhash: {},
    undelegate: {},
    lamports: {},
    finalize: {},
    data.len: {},
    commit_type: {},
    created_at: {},
    commit_status: {},
    last_retried_at: {},
    retries_count: {}
}}",
            self.reqid,
            self.pubkey,
            self.delegated_account_owner,
            self.slot,
            self.ephemeral_blockhash,
            self.undelegate,
            self.lamports,
            self.finalize,
            self.data.as_ref().map(|x| x.len()).unwrap_or_default(),
            self.commit_type.as_str(),
            self.created_at,
            self.commit_status,
            self.last_retried_at,
            self.retries_count
        )
    }
}

const ALL_COMMIT_STATUS_COLUMNS: &str = r#"
    reqid,
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
"#;

const SELECT_ALL_COMMIT_STATUS_COLUMNS: &str = r#"
SELECT
    reqid,
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
FROM commit_status
"#;

// -----------------
// Bundle Signature
// -----------------
// The BundleSignature table exists to store mappings from bundle_id to the signatures used
// to process/finalize these bundles.
// The signatures are repeated in the commit_status table, however the rows in there have a
// different lifetime than the bundle signature rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleSignatureRow {
    /// The id of the bundle that was commmitted
    /// If an account was not part of a bundle it is treated as a single account bundle
    /// for consistency.
    /// The bundle_id is unique
    pub bundle_id: u64,
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

impl BundleSignatureRow {
    pub fn new(
        bundle_id: u64,
        processed_signature: Signature,
        finalized_signature: Option<Signature>,
        undelegate_signature: Option<Signature>,
    ) -> Self {
        let created_at = now();
        Self {
            bundle_id,
            processed_signature,
            finalized_signature,
            undelegate_signature,
            created_at,
        }
    }
}

const ALL_BUNDLE_SIGNATURE_COLUMNS: &str = r#"
    bundle_id,
    processed_signature,
    finalized_signature,
    undelegate_signature,
    created_at
"#;

const SELECT_ALL_BUNDLE_SIGNATURE_COLUMNS: &str = r#"
SELECT
    bundle_id,
    processed_signature,
    finalized_signature,
    undelegate_signature,
    created_at
FROM bundle_signature
"#;

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
    pub fn update_commit_status_and_bundle_signature(
        &mut self,
        reqid: &str,
        pubkey: &Pubkey,
        status: &CommitStatus,
        bundle_signature: Option<BundleSignatureRow>,
    ) -> CommitPersistResult<()> {
        let tx = self.conn.transaction()?;
        Self::update_commit_status(&tx, reqid, pubkey, status)?;
        if let Some(bundle_signature) = bundle_signature {
            Self::insert_bundle_signature(&tx, &bundle_signature)?;
        }
        tx.commit()?;
        Ok(())
    }

    // -----------------
    // Commit Status
    // -----------------
    pub fn create_commit_status_table(&self) -> Result<()> {
        // The bundle_id is NULL when we insert a pending commit
        match self.conn.execute_batch(
            "
        BEGIN;
            CREATE TABLE IF NOT EXISTS commit_status (
                reqid                   TEXT NOT NULL,
                pubkey                  TEXT NOT NULL,
                delegated_account_owner TEXT NOT NULL,
                slot                    INTEGER NOT NULL,
                ephemeral_blockhash     TEXT NOT NULL,
                undelegate              INTEGER NOT NULL,
                lamports                INTEGER NOT NULL,
                finalize                INTEGER NOT NULL,
                bundle_id               INTEGER,
                data                    BLOB,
                commit_type             TEXT NOT NULL,
                created_at              INTEGER NOT NULL,
                commit_status           TEXT NOT NULL,
                commit_strategy         TEXT NOT NULL,
                processed_signature     TEXT,
                finalized_signature     TEXT,
                undelegated_signature   TEXT,
                last_retried_at         INTEGER NOT NULL,
                retries_count           INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_commits_pubkey ON commit_status (pubkey);
            CREATE INDEX IF NOT EXISTS idx_commits_reqid ON commit_status (reqid);
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
                commit.reqid,
                commit.pubkey.to_string(),
                commit.delegated_account_owner.to_string(),
                u64_into_i64(commit.slot),
                commit.ephemeral_blockhash.to_string(),
                if commit.undelegate { 1 } else { 0 },
                u64_into_i64(commit.lamports),
                if commit.finalize { 1 } else { 0 },
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

    fn update_commit_status(
        tx: &Transaction<'_>,
        reqid: &str,
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
                pubkey = ?7 AND reqid = ?8";
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
            reqid
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

    pub(crate) fn get_commit_statuses_by_reqid(
        &self,
        reqid: &str,
    ) -> CommitPersistResult<Vec<CommitStatusRow>> {
        let query =
            format!("{SELECT_ALL_COMMIT_STATUS_COLUMNS} WHERE reqid = ?1");
        let stmt = &mut self.conn.prepare(&query)?;
        let mut rows = stmt.query(params![reqid])?;

        extract_committor_rows(&mut rows)
    }

    pub(crate) fn get_commit_status(
        &self,
        reqid: &str,
        pubkey: &Pubkey,
    ) -> CommitPersistResult<Option<CommitStatusRow>> {
        let query = format!(
            "{SELECT_ALL_COMMIT_STATUS_COLUMNS} WHERE reqid = ?1 AND pubkey = ?2"
        );
        let stmt = &mut self.conn.prepare(&query)?;
        let mut rows = stmt.query(params![reqid, pubkey.to_string()])?;

        extract_committor_rows(&mut rows).map(|mut rows| rows.pop())
    }

    #[cfg(test)]
    fn remove_commit_statuses_with_reqid(
        &self,
        reqid: &str,
    ) -> CommitPersistResult<()> {
        let query = "DELETE FROM commit_status WHERE reqid = ?1";
        let stmt = &mut self.conn.prepare(query)?;
        stmt.execute(params![reqid])?;
        Ok(())
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
                processed_signature TEXT NOT NULL,
                finalized_signature TEXT,
                undelegate_signature TEXT,
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

    fn insert_bundle_signature(
        tx: &Transaction<'_>,
        bundle_signature: &BundleSignatureRow,
    ) -> CommitPersistResult<()> {
        let query = if bundle_signature.finalized_signature.is_some() {
            format!("INSERT OR REPLACE INTO bundle_signature ({ALL_BUNDLE_SIGNATURE_COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5)")
        } else {
            format!("INSERT OR IGNORE INTO bundle_signature ({ALL_BUNDLE_SIGNATURE_COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5)")
        };
        tx.execute(
            &query,
            params![
                bundle_signature.bundle_id,
                bundle_signature.processed_signature.to_string(),
                bundle_signature
                    .finalized_signature
                    .as_ref()
                    .map(|s| s.to_string()),
                bundle_signature
                    .undelegate_signature
                    .as_ref()
                    .map(|s| s.to_string()),
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
            let bundle_signature_row = extract_bundle_signature_row(row)?;
            Ok(Some(bundle_signature_row))
        } else {
            Ok(None)
        }
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
    let reqid: String = row.get(0)?;

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

    let finalize: bool = {
        let finalize: u8 = row.get(7)?;
        finalize == 1
    };

    let bundle_id: Option<u64> = {
        let bundle_id: Option<i64> = row.get(8)?;
        bundle_id.map(i64_into_u64)
    };

    let data: Option<Vec<u8>> = row.get(9)?;

    let commit_type = {
        let commit_type: String = row.get(10)?;
        CommitType::try_from(commit_type.as_str())?
    };
    let created_at: u64 = {
        let created_at: i64 = row.get(11)?;
        i64_into_u64(created_at)
    };
    let commit_status = {
        let commit_status: String = row.get(12)?;
        let commit_strategy = {
            let commit_strategy: String = row.get(13)?;
            CommitStrategy::from(commit_strategy.as_str())
        };
        let processed_signature = {
            let processed_signature: Option<String> = row.get(14)?;
            processed_signature
                .map(|s| Signature::from_str(s.as_str()))
                .transpose()?
        };
        let finalized_signature = {
            let finalized_signature: Option<String> = row.get(15)?;
            finalized_signature
                .map(|s| Signature::from_str(s.as_str()))
                .transpose()?
        };
        let undelegated_signature = {
            let undelegated_signature: Option<String> = row.get(16)?;
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
        let last_retried_at: i64 = row.get(17)?;
        i64_into_u64(last_retried_at)
    };
    let retries_count: u16 = {
        let retries_count: i64 = row.get(18)?;
        retries_count.try_into().unwrap_or_default()
    };

    Ok(CommitStatusRow {
        reqid,
        pubkey,
        delegated_account_owner,
        slot,
        ephemeral_blockhash,
        undelegate,
        lamports,
        finalize,
        data,
        commit_type,
        created_at,
        commit_status,
        last_retried_at,
        retries_count,
    })
}

// -----------------
// Bundle Signature Helpers
// -----------------
fn extract_bundle_signature_row(
    row: &rusqlite::Row,
) -> CommitPersistResult<BundleSignatureRow> {
    let bundle_id: u64 = {
        let bundle_id: i64 = row.get(0)?;
        i64_into_u64(bundle_id)
    };
    let processed_signature = {
        let processed_signature: String = row.get(1)?;
        Signature::from_str(processed_signature.as_str())?
    };
    let finalized_signature = {
        let finalized_signature: Option<String> = row.get(2)?;
        finalized_signature
            .map(|s| Signature::from_str(s.as_str()))
            .transpose()?
    };
    let undelegate_signature = {
        let undelegate_signature: Option<String> = row.get(3)?;
        undelegate_signature
            .map(|s| Signature::from_str(s.as_str()))
            .transpose()?
    };
    let created_at: u64 = {
        let created_at: i64 = row.get(4)?;
        i64_into_u64(created_at)
    };

    Ok(BundleSignatureRow {
        bundle_id,
        processed_signature,
        finalized_signature,
        undelegate_signature,
        created_at,
    })
}

#[cfg(test)]
mod test {
    use super::*;

    fn setup_db() -> CommittorDb {
        let db = CommittorDb::new(":memory:").unwrap();
        db.create_commit_status_table().unwrap();
        db.create_bundle_signature_table().unwrap();
        db
    }

    // -----------------
    // Commit Status
    // -----------------
    fn create_commit_status_row(reqid: &str) -> CommitStatusRow {
        CommitStatusRow {
            reqid: reqid.to_string(),
            pubkey: Pubkey::new_unique(),
            delegated_account_owner: Pubkey::new_unique(),
            slot: 100,
            ephemeral_blockhash: Hash::new_unique(),
            undelegate: false,
            lamports: 100,
            finalize: true,
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
            reqid: "req-123".to_string(),
            pubkey: Pubkey::new_unique(),
            delegated_account_owner: Pubkey::new_unique(),
            slot: 100,
            ephemeral_blockhash: Hash::new_unique(),
            undelegate: false,
            lamports: 100,
            finalize: true,
            data: None,
            commit_type: CommitType::EmptyAccount,
            created_at: 1000,
            commit_status: CommitStatus::Pending,
            last_retried_at: 1000,
            retries_count: 0,
        };

        let two_bundled_commit_row_with_data = CommitStatusRow {
            reqid: "req-123".to_string(),
            pubkey: Pubkey::new_unique(),
            delegated_account_owner: Pubkey::new_unique(),
            slot: 100,
            ephemeral_blockhash: Hash::new_unique(),
            undelegate: false,
            lamports: 2000,
            finalize: true,
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

        let by_reqid = db
            .get_commit_statuses_by_reqid(
                &one_unbundled_commit_row_no_data.reqid,
            )
            .unwrap();
        assert_eq!(by_reqid.len(), 2);
        assert_eq!(
            by_reqid,
            [
                one_unbundled_commit_row_no_data,
                two_bundled_commit_row_with_data
            ]
        );
    }

    #[test]
    fn test_commits_with_reqid() {
        let mut db = setup_db();
        const REQID_ONE: &str = "req-123";
        const REQID_TWO: &str = "req-456";

        let commit_row_one = create_commit_status_row(REQID_ONE);
        let commit_row_one_other = create_commit_status_row(REQID_ONE);
        let commit_row_two = create_commit_status_row(REQID_TWO);
        db.insert_commit_status_rows(&[
            commit_row_one.clone(),
            commit_row_one_other.clone(),
            commit_row_two.clone(),
        ])
        .unwrap();

        let commits_one = db.get_commit_statuses_by_reqid(REQID_ONE).unwrap();
        assert_eq!(commits_one.len(), 2);
        assert_eq!(commits_one[0], commit_row_one);
        assert_eq!(commits_one[1], commit_row_one_other);

        let commits_two = db.get_commit_statuses_by_reqid(REQID_TWO).unwrap();
        assert_eq!(commits_two.len(), 1);
        assert_eq!(commits_two[0], commit_row_two);

        // Remove commits with REQID_ONE
        db.remove_commit_statuses_with_reqid(REQID_ONE).unwrap();
        let commits_one_after_removal =
            db.get_commit_statuses_by_reqid(REQID_ONE).unwrap();
        assert_eq!(commits_one_after_removal.len(), 0);

        let commits_two_after_removal =
            db.get_commit_statuses_by_reqid(REQID_TWO).unwrap();
        assert_eq!(commits_two_after_removal.len(), 1);
    }

    // -----------------
    // Bundle Signature and Commit Status Updates
    // -----------------
    fn create_bundle_signature_row(
        commit_status: &CommitStatus,
    ) -> Option<BundleSignatureRow> {
        commit_status
            .bundle_id()
            .map(|bundle_id| BundleSignatureRow {
                bundle_id,
                processed_signature: Signature::new_unique(),
                finalized_signature: None,
                undelegate_signature: None,
                created_at: 1000,
            })
    }

    #[test]
    fn test_upsert_bundle_signature() {
        let mut db = setup_db();

        let process_only =
            BundleSignatureRow::new(1, Signature::new_unique(), None, None);
        let process_finalize_and_undelegate = BundleSignatureRow::new(
            2,
            Signature::new_unique(),
            Some(Signature::new_unique()),
            Some(Signature::new_unique()),
        );

        // Add two rows, one with finalize and undelegate signatures
        {
            let tx = db.conn.transaction().unwrap();
            CommittorDb::insert_bundle_signature(&tx, &process_only).unwrap();
            CommittorDb::insert_bundle_signature(
                &tx,
                &process_finalize_and_undelegate,
            )
            .unwrap();
            tx.commit().unwrap();
        }

        // Ensure we update with finalized and undelegate sigs
        let process_now_with_finalize_and_undelegate = {
            let tx = db.conn.transaction().unwrap();
            let process_now_with_finalize = BundleSignatureRow::new(
                process_only.bundle_id,
                process_finalize_and_undelegate.processed_signature,
                Some(Signature::new_unique()),
                Some(Signature::new_unique()),
            );
            CommittorDb::insert_bundle_signature(
                &tx,
                &process_now_with_finalize,
            )
            .unwrap();
            tx.commit().unwrap();

            process_now_with_finalize
        };
        assert_eq!(
            db.get_bundle_signature_by_bundle_id(1).unwrap().unwrap(),
            process_now_with_finalize_and_undelegate
        );

        // Ensure we don't erase finalized/undelegate sigs
        {
            let tx = db.conn.transaction().unwrap();
            let finalizes_now_only_process = BundleSignatureRow::new(
                process_finalize_and_undelegate.bundle_id,
                process_finalize_and_undelegate.processed_signature,
                None,
                None,
            );
            CommittorDb::insert_bundle_signature(
                &tx,
                &finalizes_now_only_process,
            )
            .unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(
            db.get_bundle_signature_by_bundle_id(2).unwrap().unwrap(),
            process_finalize_and_undelegate
        );
    }

    #[test]
    fn test_update_commit_status() {
        let mut db = setup_db();
        const REQID: &str = "req-123";

        let failing_commit_row = create_commit_status_row(REQID);
        let success_commit_row = create_commit_status_row(REQID);
        db.insert_commit_status_rows(&[
            failing_commit_row.clone(),
            success_commit_row.clone(),
        ])
        .unwrap();

        // Update the statuses
        let new_failing_status =
            CommitStatus::FailedProcess((22, CommitStrategy::FromBuffer, None));
        db.update_commit_status_and_bundle_signature(
            &failing_commit_row.reqid,
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
        let success_signatures_row =
            create_bundle_signature_row(&new_success_status);
        let success_signatures = success_signatures_row.clone().unwrap();
        db.update_commit_status_and_bundle_signature(
            &success_commit_row.reqid,
            &success_commit_row.pubkey,
            &new_success_status,
            success_signatures_row,
        )
        .unwrap();

        // Verify the statuses were updated
        let failed_commit_row = db
            .get_commit_status(REQID, &failing_commit_row.pubkey)
            .unwrap()
            .unwrap();
        assert_eq!(failed_commit_row.commit_status, new_failing_status);

        let succeeded_commit_row = db
            .get_commit_status(REQID, &success_commit_row.pubkey)
            .unwrap()
            .unwrap();
        assert_eq!(succeeded_commit_row.commit_status, new_success_status);
        let signature_row =
            db.get_bundle_signature_by_bundle_id(33).unwrap().unwrap();
        assert_eq!(
            signature_row.processed_signature,
            success_signatures.processed_signature,
        );
        assert_eq!(signature_row.finalized_signature, None);
    }
}
