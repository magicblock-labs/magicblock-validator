use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::Utc;
use magicblock_program::args::ScheduleTaskRequest;
use rusqlite::{Connection, OptionalExtension, params};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use tokio::sync::Mutex;

use crate::errors::TaskSchedulerResult;

/// Represents a task in the database
/// Uses i64 for all timestamps and IDs to avoid overflows
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbTask {
    /// Unique identifier for this task
    pub id: i64,
    /// Instructions to execute when triggered
    pub instructions: Vec<Instruction>,
    /// Authority that can modify or cancel this task
    pub authority: Pubkey,
    /// How frequently the task should be executed, in milliseconds
    pub execution_interval_millis: i64,
    /// Number of times this task still needs to be executed.
    pub executions_left: i64,
    /// Timestamp of the last execution of this task in milliseconds since UNIX epoch
    pub last_execution_millis: i64,
    /// Timestamp of the latest persisted mutation of this task in milliseconds since UNIX epoch
    pub updated_at: i64,
}

impl From<ScheduleTaskRequest> for DbTask {
    fn from(task: ScheduleTaskRequest) -> Self {
        Self {
            id: task.id,
            instructions: task.instructions,
            authority: task.authority,
            execution_interval_millis: task.execution_interval_millis,
            executions_left: task.iterations,
            last_execution_millis: 0,
            updated_at: 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CrankSuccessUpdate {
    /// Task whose successful execution should be persisted.
    pub task_id: i64,
    /// Actual execution timestamp to store in `tasks.last_execution_millis`.
    pub last_execution_millis: i64,
    /// Optimistic concurrency token. The update is applied only if the current
    /// row still has this `tasks.updated_at` value.
    pub expected_updated_at: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct CrankSuccessRemoval {
    /// Task whose final successful execution should remove it from `tasks`.
    pub task_id: i64,
    /// Optimistic concurrency token. The removal is applied only if the current
    /// row still has this `tasks.updated_at` value.
    pub expected_updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct CrankFailedMove {
    /// Task to remove from `tasks` and append to `failed_tasks`.
    pub task_id: i64,
    /// Optimistic concurrency token. The move is applied only if the current
    /// row still has this `tasks.updated_at` value.
    pub expected_updated_at: i64,
    /// Error text persisted in `failed_tasks.error` when the move succeeds.
    pub error: String,
}

#[derive(Debug, Clone, Copy)]
pub struct CrankRetryCheck {
    /// Task that failed but remains retryable.
    pub task_id: i64,
    /// Optimistic concurrency token. The retry is considered valid only if the
    /// current row still has this `tasks.updated_at` value.
    pub expected_updated_at: i64,
}

/// Applied metadata for a successful task execution that remains scheduled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrankSuccessUpdateCompletion {
    /// Optimistic token supplied by the caller and matched against the previous
    /// `tasks.updated_at` value.
    pub expected_updated_at: i64,
    /// New `tasks.updated_at` value written by the database transaction.
    pub new_updated_at: i64,
}

/// Applied metadata for a task removed after its final successful execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrankSuccessRemovalCompletion {
    /// Optimistic token supplied by the caller and matched against the previous
    /// `tasks.updated_at` value before deleting the row.
    pub expected_updated_at: i64,
}

/// Applied metadata for a task moved from `tasks` to `failed_tasks`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrankFailedMoveCompletion {
    /// Optimistic token supplied by the caller and matched against the previous
    /// `tasks.updated_at` value before moving the task.
    pub expected_updated_at: i64,
}

/// Applied metadata for a retryable failed task that still matches its DB row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrankRetryCheckCompletion {
    /// Actual `tasks.updated_at` value read from the database. This equals the
    /// requested optimistic token when the retry check succeeds.
    pub current_updated_at: i64,
}

/// Results for a crank batch transaction keyed by task ID.
///
/// Each map contains only entries whose optimistic `expected_updated_at` token
/// matched the database state and whose corresponding DB operation succeeded.
#[derive(Debug, Default)]
pub struct CrankBatchCompletion {
    /// Continued successful executions. Values include both the matched
    /// optimistic token and the new `tasks.updated_at` written by the DB.
    pub success_updates: HashMap<i64, CrankSuccessUpdateCompletion>,
    /// Final successful executions removed from `tasks`. Values contain the
    /// matched optimistic token used for the delete.
    pub success_removals: HashMap<i64, CrankSuccessRemovalCompletion>,
    /// Failed executions moved to `failed_tasks`. Values contain the matched
    /// optimistic token used for the move.
    pub failed_moves: HashMap<i64, CrankFailedMoveCompletion>,
    /// Retryable failed executions whose `tasks` row still exists unchanged.
    /// Values contain the actual `tasks.updated_at` read from the DB.
    pub retry_checks: HashMap<i64, CrankRetryCheckCompletion>,
}

#[derive(Debug, Clone)]
pub struct FailedScheduling {
    pub id: i64,
    pub timestamp: i64,
    pub task_id: i64,
    pub error: String,
}

#[derive(Debug, Clone)]
pub struct FailedTask {
    pub id: i64,
    pub timestamp: i64,
    pub task_id: i64,
    pub error: String,
}

#[derive(Clone)]
pub struct SchedulerDatabase {
    conn: Arc<Mutex<Connection>>,
}

impl SchedulerDatabase {
    pub fn path(path: &Path) -> PathBuf {
        path.join("task_scheduler.sqlite")
    }

    pub fn new<P: AsRef<Path>>(path: P) -> TaskSchedulerResult<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=NORMAL;
            PRAGMA busy_timeout=5000;
            PRAGMA cache_size=-65536;
            ",
        )?;

        // Create tables
        conn.execute(
            "CREATE TABLE IF NOT EXISTS tasks (
                id INTEGER PRIMARY KEY,
                instructions BLOB NOT NULL,
                authority TEXT NOT NULL,
                execution_interval_millis INTEGER NOT NULL,
                executions_left INTEGER NOT NULL,
                last_execution_millis INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS failed_scheduling (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp INTEGER NOT NULL,
                task_id INTEGER,
                error TEXT NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS failed_tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp INTEGER NOT NULL,
                task_id INTEGER,
                error TEXT NOT NULL
            )",
            [],
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub async fn insert_task(&self, task: &DbTask) -> TaskSchedulerResult<i64> {
        let instructions_bin = wincode::serialize(&task.instructions)?;
        let authority_str = task.authority.to_string();
        let conn = self.conn.lock().await;
        let previous_updated_at: Option<i64> = conn
            .query_row(
                "SELECT updated_at FROM tasks WHERE id = ?",
                [task.id],
                |row| row.get(0),
            )
            .optional()?;
        let now = previous_updated_at
            .map(|updated_at| Utc::now().timestamp_millis().max(updated_at + 1))
            .unwrap_or_else(|| Utc::now().timestamp_millis());

        conn.execute(
            "INSERT OR REPLACE INTO tasks 
             (id, instructions, authority, execution_interval_millis, executions_left, last_execution_millis, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                task.id,
                instructions_bin,
                authority_str,
                task.execution_interval_millis,
                task.executions_left,
                task.last_execution_millis,
                now,
                now,
            ],
        )?;

        Ok(now)
    }

    pub async fn update_task_after_execution(
        &self,
        task_id: i64,
        last_execution: i64,
    ) -> TaskSchedulerResult<()> {
        let now = Utc::now().timestamp_millis();

        self.conn.lock().await.execute(
            "UPDATE tasks 
             SET executions_left = executions_left - 1, 
                 last_execution_millis = ?,
                 updated_at = ?
             WHERE id = ?",
            params![last_execution, now, task_id],
        )?;

        Ok(())
    }

    pub async fn insert_failed_scheduling(
        &self,
        task_id: i64,
        error: String,
    ) -> TaskSchedulerResult<()> {
        self.conn.lock().await.execute(
            "INSERT INTO failed_scheduling (timestamp, task_id, error) VALUES (?, ?, ?)",
            params![Utc::now().timestamp_millis(), task_id, error],
        )?;

        Ok(())
    }

    pub async fn insert_failed_task(
        &self,
        task_id: i64,
        error: String,
    ) -> TaskSchedulerResult<()> {
        self.conn.lock().await.execute(
            "INSERT INTO failed_tasks (timestamp, task_id, error) VALUES (?, ?, ?)",
            params![Utc::now().timestamp_millis(), task_id, error],
        )?;

        Ok(())
    }

    pub async fn unschedule_task(
        &self,
        task_id: i64,
    ) -> TaskSchedulerResult<()> {
        self.conn.lock().await.execute(
            "UPDATE tasks SET executions_left = 0 WHERE id = ?",
            [task_id],
        )?;

        Ok(())
    }

    pub async fn remove_task(&self, task_id: i64) -> TaskSchedulerResult<()> {
        self.conn
            .lock()
            .await
            .execute("DELETE FROM tasks WHERE id = ?", [task_id])?;

        Ok(())
    }

    pub async fn get_task(
        &self,
        task_id: i64,
    ) -> TaskSchedulerResult<Option<DbTask>> {
        let db = self.conn.lock().await;
        let mut stmt = db.prepare(
            "SELECT id, instructions, authority, execution_interval_millis, executions_left, last_execution_millis,
             updated_at
             FROM tasks WHERE id = ?"
        )?;

        let mut rows = stmt.query_map([task_id], |row| {
            let instructions_bin: Vec<u8> = row.get(1)?;
            let instructions: Vec<Instruction> =
                wincode::deserialize(&instructions_bin).map_err(|e| {
                    rusqlite::Error::InvalidParameterName(format!(
                        "instructions: {}",
                        e
                    ))
                })?;
            let authority_str: String = row.get(2)?;
            let authority: Pubkey = authority_str.parse().map_err(|e| {
                rusqlite::Error::InvalidParameterName(format!(
                    "authority: {}",
                    e
                ))
            })?;

            Ok(DbTask {
                id: row.get(0)?,
                instructions,
                authority,
                execution_interval_millis: row.get(3)?,
                executions_left: row.get(4)?,
                last_execution_millis: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?;

        Ok(rows.next().transpose()?)
    }

    pub async fn get_tasks(&self) -> TaskSchedulerResult<Vec<DbTask>> {
        let db = self.conn.lock().await;
        let mut stmt = db.prepare(
            "SELECT id, instructions, authority, execution_interval_millis, executions_left, last_execution_millis,
             updated_at
             FROM tasks"
        )?;

        let mut tasks = Vec::new();
        let rows = stmt.query_map([], |row| {
            let instructions_bin: Vec<u8> = row.get(1)?;
            let instructions: Vec<Instruction> =
                wincode::deserialize(&instructions_bin).map_err(|e| {
                    rusqlite::Error::InvalidParameterName(format!(
                        "instructions: {}",
                        e
                    ))
                })?;
            let authority_str: String = row.get(2)?;
            let authority: Pubkey = authority_str.parse().map_err(|e| {
                rusqlite::Error::InvalidParameterName(format!(
                    "authority: {}",
                    e
                ))
            })?;

            Ok(DbTask {
                id: row.get(0)?,
                instructions,
                authority,
                execution_interval_millis: row.get(3)?,
                executions_left: row.get(4)?,
                last_execution_millis: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?;

        for row in rows {
            tasks.push(row?);
        }

        Ok(tasks)
    }

    pub async fn get_task_ids(&self) -> TaskSchedulerResult<Vec<i64>> {
        let db = self.conn.lock().await;
        let mut stmt = db.prepare(
            "SELECT id 
             FROM tasks",
        )?;

        let rows = stmt.query_map([], |row| row.get(0))?;

        Ok(rows.collect::<Result<Vec<i64>, rusqlite::Error>>()?)
    }

    pub async fn get_failed_schedulings(
        &self,
    ) -> TaskSchedulerResult<Vec<FailedScheduling>> {
        let db = self.conn.lock().await;
        let mut stmt = db.prepare(
            "SELECT * 
             FROM failed_scheduling",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(FailedScheduling {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                task_id: row.get(2)?,
                error: row.get(3)?,
            })
        })?;

        Ok(rows.collect::<Result<Vec<FailedScheduling>, rusqlite::Error>>()?)
    }

    pub async fn get_failed_tasks(
        &self,
    ) -> TaskSchedulerResult<Vec<FailedTask>> {
        let db = self.conn.lock().await;
        let mut stmt = db.prepare(
            "SELECT * 
             FROM failed_tasks",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(FailedTask {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                task_id: row.get(2)?,
                error: row.get(3)?,
            })
        })?;

        Ok(rows.collect::<Result<Vec<FailedTask>, rusqlite::Error>>()?)
    }

    /// One transaction for a crank completion batch — replaces N separate commits from
    /// per-task `update_task_after_execution` / `remove_task` / `move_task_to_failed` calls.
    pub async fn apply_crank_batch_completion(
        &self,
        success_updates: &[CrankSuccessUpdate],
        success_removals: &[CrankSuccessRemoval],
        failed_moves: &[CrankFailedMove],
        retry_checks: &[CrankRetryCheck],
    ) -> TaskSchedulerResult<CrankBatchCompletion> {
        if success_updates.is_empty()
            && success_removals.is_empty()
            && failed_moves.is_empty()
            && retry_checks.is_empty()
        {
            return Ok(CrankBatchCompletion::default());
        }

        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;
        let mut completion = CrankBatchCompletion::default();

        // Continued executions — decrement executions_left via existing UPDATE semantics
        for update in success_updates {
            let now = Utc::now()
                .timestamp_millis()
                .max(update.expected_updated_at + 1);
            let affected = tx.execute(
                "UPDATE tasks SET executions_left = executions_left - 1,
                     last_execution_millis = ?, updated_at = ?
                 WHERE id = ? AND updated_at = ?",
                params![
                    update.last_execution_millis,
                    now,
                    update.task_id,
                    update.expected_updated_at
                ],
            )?;
            if affected == 1 {
                completion.success_updates.insert(
                    update.task_id,
                    CrankSuccessUpdateCompletion {
                        expected_updated_at: update.expected_updated_at,
                        new_updated_at: now,
                    },
                );
            }
        }

        for removal in success_removals {
            let affected = tx.execute(
                "DELETE FROM tasks WHERE id = ? AND updated_at = ?",
                params![removal.task_id, removal.expected_updated_at],
            )?;
            if affected == 1 {
                completion.success_removals.insert(
                    removal.task_id,
                    CrankSuccessRemovalCompletion {
                        expected_updated_at: removal.expected_updated_at,
                    },
                );
            }
        }

        for failed in failed_moves {
            let affected = tx.execute(
                "DELETE FROM tasks WHERE id = ? AND updated_at = ?",
                params![failed.task_id, failed.expected_updated_at],
            )?;
            if affected == 1 {
                let now = Utc::now().timestamp_millis();
                tx.execute(
                    "INSERT INTO failed_tasks (timestamp, task_id, error)
                     VALUES (?, ?, ?)",
                    params![now, failed.task_id, failed.error],
                )?;
                completion.failed_moves.insert(
                    failed.task_id,
                    CrankFailedMoveCompletion {
                        expected_updated_at: failed.expected_updated_at,
                    },
                );
            }
        }

        for check in retry_checks {
            let matched: Option<i64> = tx
                .query_row(
                    "SELECT updated_at FROM tasks
                     WHERE id = ? AND updated_at = ?",
                    params![check.task_id, check.expected_updated_at],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(updated_at) = matched {
                completion.retry_checks.insert(
                    check.task_id,
                    CrankRetryCheckCompletion {
                        current_updated_at: updated_at,
                    },
                );
            }
        }

        tx.commit()?;
        Ok(completion)
    }

    pub async fn delete_failed_records_older_than(
        &self,
        cutoff_timestamp_millis: i64,
    ) -> TaskSchedulerResult<usize> {
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;
        let failed_scheduling_deleted = tx.execute(
            "DELETE FROM failed_scheduling WHERE timestamp < ?",
            [cutoff_timestamp_millis],
        )?;
        let failed_tasks_deleted = tx.execute(
            "DELETE FROM failed_tasks WHERE timestamp < ?",
            [cutoff_timestamp_millis],
        )?;
        tx.commit()?;

        Ok(failed_scheduling_deleted + failed_tasks_deleted)
    }
}
