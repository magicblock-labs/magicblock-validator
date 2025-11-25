use std::path::{Path, PathBuf};

use chrono::Utc;
use magicblock_program::args::ScheduleTaskRequest;
use rusqlite::{params, Connection};
use solana_sdk::{instruction::Instruction, pubkey::Pubkey};
use tokio::sync::Mutex;

use crate::errors::TaskSchedulerError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbTask {
    /// Unique identifier for this task
    pub id: u64,
    /// Instructions to execute when triggered
    pub instructions: Vec<Instruction>,
    /// Authority that can modify or cancel this task
    pub authority: Pubkey,
    /// How frequently the task should be executed, in milliseconds
    pub execution_interval_millis: u64,
    /// Number of times this task still needs to be executed.
    pub executions_left: u64,
    /// Timestamp of the last execution of this task in milliseconds since UNIX epoch
    pub last_execution_millis: u64,
}

impl<'a> From<&'a ScheduleTaskRequest> for DbTask {
    fn from(task: &'a ScheduleTaskRequest) -> Self {
        Self {
            id: task.id,
            instructions: task.instructions.clone(),
            authority: task.authority,
            execution_interval_millis: task.execution_interval_millis,
            executions_left: task.iterations,
            last_execution_millis: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FailedScheduling {
    pub id: u64,
    pub timestamp: u64,
    pub task_id: u64,
    pub error: String,
}

#[derive(Debug, Clone)]
pub struct FailedTask {
    pub id: u64,
    pub timestamp: u64,
    pub task_id: u64,
    pub error: String,
}

pub struct SchedulerDatabase {
    conn: Mutex<Connection>,
}

impl SchedulerDatabase {
    pub fn path(path: &Path) -> PathBuf {
        path.join("task_scheduler.sqlite")
    }

    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, TaskSchedulerError> {
        let conn = Connection::open(path)?;

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
            conn: Mutex::new(conn),
        })
    }

    pub async fn insert_task(
        &self,
        task: &DbTask,
    ) -> Result<(), TaskSchedulerError> {
        let instructions_bin = bincode::serialize(&task.instructions)?;
        let authority_str = task.authority.to_string();
        let now = Utc::now().timestamp_millis();

        self.conn.lock().await.execute(
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

        Ok(())
    }

    pub async fn update_task_after_execution(
        &self,
        task_id: u64,
        last_execution: i64,
    ) -> Result<(), TaskSchedulerError> {
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
        task_id: u64,
        error: String,
    ) -> Result<(), TaskSchedulerError> {
        self.conn.lock().await.execute(
            "INSERT INTO failed_scheduling (timestamp, task_id, error) VALUES (?, ?, ?)",
            params![Utc::now().timestamp_millis(), task_id, error],
        )?;

        Ok(())
    }

    pub async fn insert_failed_task(
        &self,
        task_id: u64,
        error: String,
    ) -> Result<(), TaskSchedulerError> {
        self.conn.lock().await.execute(
            "INSERT INTO failed_tasks (timestamp, task_id, error) VALUES (?, ?, ?)",
            params![Utc::now().timestamp_millis(), task_id, error],
        )?;

        Ok(())
    }

    pub async fn unschedule_task(
        &self,
        task_id: u64,
    ) -> Result<(), TaskSchedulerError> {
        self.conn.lock().await.execute(
            "UPDATE tasks SET executions_left = 0 WHERE id = ?",
            [task_id],
        )?;

        Ok(())
    }

    pub async fn remove_task(
        &self,
        task_id: u64,
    ) -> Result<(), TaskSchedulerError> {
        self.conn
            .lock()
            .await
            .execute("DELETE FROM tasks WHERE id = ?", [task_id])?;

        Ok(())
    }

    pub async fn get_task(
        &self,
        task_id: u64,
    ) -> Result<Option<DbTask>, TaskSchedulerError> {
        let db = self.conn.lock().await;
        let mut stmt = db.prepare(
            "SELECT id, instructions, authority, execution_interval_millis, executions_left, last_execution_millis
             FROM tasks WHERE id = ?"
        )?;

        let mut rows = stmt.query_map([task_id], |row| {
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
                last_execution_millis: row.get(5)?,
            })
        })?;

        Ok(rows.next().transpose()?)
    }

    pub async fn get_tasks(&self) -> Result<Vec<DbTask>, TaskSchedulerError> {
        let db = self.conn.lock().await;
        let mut stmt = db.prepare(
            "SELECT id, instructions, authority, execution_interval_millis, executions_left, last_execution_millis
             FROM tasks"
        )?;

        let mut tasks = Vec::new();
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
                last_execution_millis: row.get(5)?,
            })
        })?;

        for row in rows {
            tasks.push(row?);
        }

        Ok(tasks)
    }

    pub async fn get_task_ids(&self) -> Result<Vec<u64>, TaskSchedulerError> {
        let db = self.conn.lock().await;
        let mut stmt = db.prepare(
            "SELECT id 
             FROM tasks",
        )?;

        let rows = stmt.query_map([], |row| row.get(0))?;

        Ok(rows.collect::<Result<Vec<u64>, rusqlite::Error>>()?)
    }

    pub async fn get_failed_schedulings(
        &self,
    ) -> Result<Vec<FailedScheduling>, TaskSchedulerError> {
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
    ) -> Result<Vec<FailedTask>, TaskSchedulerError> {
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
}
