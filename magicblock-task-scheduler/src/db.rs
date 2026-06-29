use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::Utc;
use magicblock_program::args::ScheduleTaskRequest;
use rusqlite::{params, Connection};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use tokio::sync::Mutex;

use crate::errors::TaskSchedulerResult;

/// Represents a task in the database
/// Uses i64 for all timestamps and IDs to avoid overflows
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbTask {
    /// Unique identifier for this task.
    pub id: i64,
    /// Instructions to execute when triggered.
    pub instructions: Vec<Instruction>,
    /// Authority that owns this task.
    pub authority: Pubkey,
    /// How frequently the task should be executed, in milliseconds.
    pub execution_interval_millis: i64,
    /// Number of times this task still needs to be executed.
    pub executions_left: i64,
}

impl From<ScheduleTaskRequest> for DbTask {
    fn from(task: ScheduleTaskRequest) -> Self {
        Self {
            id: task.id,
            instructions: task.instructions,
            authority: task.authority,
            execution_interval_millis: task.execution_interval_millis,
            executions_left: task.iterations,
        }
    }
}

/// Migration-only store over the legacy scheduler's SQLite database.
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
            ",
        )?;

        // Mirrors the legacy schema so existing databases can be read. The
        // timestamp columns are retained for compatibility but are not used by
        // migration.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS tasks (
                id INTEGER PRIMARY KEY,
                instructions BLOB NOT NULL,
                authority TEXT NOT NULL,
                execution_interval_millis INTEGER NOT NULL,
                executions_left INTEGER NOT NULL,
                last_execution_millis INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Inserts (or replaces) a task. Used to seed legacy tasks, including from
    /// tests exercising migration.
    pub async fn insert_task(&self, task: &DbTask) -> TaskSchedulerResult<()> {
        let instructions_bin = bincode::serialize(&task.instructions)?;
        let authority_str = task.authority.to_string();
        let now = Utc::now().timestamp_millis();
        self.conn.lock().await.execute(
            "INSERT OR REPLACE INTO tasks
             (id, instructions, authority, execution_interval_millis, executions_left, last_execution_millis, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, 0, ?, ?)",
            params![
                task.id,
                instructions_bin,
                authority_str,
                task.execution_interval_millis,
                task.executions_left,
                now,
                now,
            ],
        )?;
        Ok(())
    }

    /// Returns all persisted tasks awaiting migration.
    pub async fn get_tasks(&self) -> TaskSchedulerResult<Vec<DbTask>> {
        let db = self.conn.lock().await;
        let mut stmt = db.prepare(
            "SELECT id, instructions, authority, execution_interval_millis, executions_left
             FROM tasks",
        )?;

        let rows = stmt.query_map([], |row| {
            let instructions_bin: Vec<u8> = row.get(1)?;
            let instructions: Vec<Instruction> =
                bincode::deserialize(&instructions_bin).map_err(|e| {
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
            })
        })?;

        Ok(rows.collect::<Result<Vec<DbTask>, rusqlite::Error>>()?)
    }

    /// Returns the ids of all persisted tasks. Used by tests to assert the
    /// database empties after migration.
    pub async fn get_task_ids(&self) -> TaskSchedulerResult<Vec<i64>> {
        let db = self.conn.lock().await;
        let mut stmt = db.prepare("SELECT id FROM tasks")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<Result<Vec<i64>, rusqlite::Error>>()?)
    }

    /// Removes a task once it has been migrated onto hydra.
    pub async fn remove_task(&self, task_id: i64) -> TaskSchedulerResult<()> {
        self.conn
            .lock()
            .await
            .execute("DELETE FROM tasks WHERE id = ?", [task_id])?;
        Ok(())
    }
}
