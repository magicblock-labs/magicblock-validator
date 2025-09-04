use std::path::{Path, PathBuf};

use chrono::Utc;
use log::*;
use rusqlite::{params, Connection};
use solana_sdk::{instruction::Instruction, pubkey::Pubkey};

use crate::errors::TaskSchedulerError;

#[derive(Debug, Clone)]
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

impl PartialEq for DbTask {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.instructions == other.instructions
            && self.authority == other.authority
            && self.execution_interval_millis == other.execution_interval_millis
            && self.executions_left == other.executions_left
            && self.last_execution_millis == other.last_execution_millis
    }
}

impl Eq for DbTask {}

#[derive(Debug, Clone)]
pub struct FailedScheduling {
    pub id: u64,
}

#[derive(Debug, Clone)]
pub struct FailedTask {
    pub id: u64,
}

pub struct SchedulerDatabase {
    conn: Connection,
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
                id INTEGER PRIMARY KEY
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS failed_tasks (
                id INTEGER PRIMARY KEY
            )",
            [],
        )?;

        Ok(Self { conn })
    }

    pub fn insert_task(&self, task: &DbTask) -> Result<(), TaskSchedulerError> {
        let instructions_bin = bincode::serialize(&task.instructions)?;
        let authority_str = task.authority.to_string();
        let now = Utc::now().timestamp_millis();

        self.conn.execute(
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

        trace!("Inserted task {} into database", task.id);
        Ok(())
    }

    pub fn update_task_after_execution(
        &self,
        task_id: u64,
        last_execution: i64,
    ) -> Result<(), TaskSchedulerError> {
        let now = Utc::now().timestamp_millis();

        self.conn.execute(
            "UPDATE tasks 
             SET executions_left = executions_left - 1, 
                 last_execution_millis = ?,
                 updated_at = ?
             WHERE id = ?",
            params![last_execution, now, task_id],
        )?;

        trace!("Updated task {} after execution", task_id);
        Ok(())
    }

    pub fn insert_failed_scheduling(
        &self,
        failed_scheduling: &FailedScheduling,
    ) -> Result<(), TaskSchedulerError> {
        self.conn.execute(
            "INSERT INTO failed_scheduling (id) VALUES (?)",
            [failed_scheduling.id],
        )?;
        trace!(
            "Inserted failed scheduling {} into database",
            failed_scheduling.id
        );
        Ok(())
    }

    pub fn remove_failed_scheduling(
        &self,
        task_id: u64,
    ) -> Result<(), TaskSchedulerError> {
        self.conn
            .execute("DELETE FROM failed_scheduling WHERE id = ?", [task_id])?;
        trace!("Removed failed scheduling {} from database", task_id);
        Ok(())
    }

    pub fn insert_failed_task(
        &self,
        failed_task: &FailedTask,
    ) -> Result<(), TaskSchedulerError> {
        let changed_rows = self.conn.execute(
            "INSERT INTO failed_tasks (id) VALUES (?)",
            [failed_task.id],
        )?;
        trace!("Changed rows: {}", changed_rows);
        trace!("Failed task ids: {:?}", self.get_failed_task_ids()?);
        trace!("Inserted failed task {} into database", failed_task.id);
        Ok(())
    }

    pub fn remove_failed_task(
        &self,
        task_id: u64,
    ) -> Result<(), TaskSchedulerError> {
        self.conn
            .execute("DELETE FROM failed_tasks WHERE id = ?", [task_id])?;
        trace!("Removed failed task {} from database", task_id);
        Ok(())
    }

    pub fn unschedule_task(
        &self,
        task_id: u64,
    ) -> Result<(), TaskSchedulerError> {
        self.conn.execute(
            "UPDATE tasks SET executions_left = 0 WHERE id = ?",
            [task_id],
        )?;
        trace!("Unscheduled task {}", task_id);
        Ok(())
    }

    pub fn remove_task(&self, task_id: u64) -> Result<(), TaskSchedulerError> {
        self.conn
            .execute("DELETE FROM tasks WHERE id = ?", [task_id])?;
        trace!("Removed task {} from database", task_id);
        Ok(())
    }

    pub fn get_task(
        &self,
        task_id: u64,
    ) -> Result<Option<DbTask>, TaskSchedulerError> {
        let mut stmt = self.conn.prepare(
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

    pub fn get_tasks(&self) -> Result<Vec<DbTask>, TaskSchedulerError> {
        let mut stmt = self.conn.prepare(
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

    pub fn get_task_ids(&self) -> Result<Vec<u64>, TaskSchedulerError> {
        let mut stmt = self.conn.prepare(
            "SELECT id 
             FROM tasks",
        )?;

        let rows = stmt.query_map([], |row| row.get(0))?;

        Ok(rows.collect::<Result<Vec<u64>, rusqlite::Error>>()?)
    }

    pub fn get_failed_scheduling_ids(
        &self,
    ) -> Result<Vec<u64>, TaskSchedulerError> {
        let mut stmt = self.conn.prepare(
            "SELECT id 
             FROM failed_scheduling",
        )?;

        let rows = stmt.query_map([], |row| row.get(0))?;

        Ok(rows.collect::<Result<Vec<u64>, rusqlite::Error>>()?)
    }

    pub fn get_failed_task_ids(&self) -> Result<Vec<u64>, TaskSchedulerError> {
        let mut stmt = self.conn.prepare(
            "SELECT id 
             FROM failed_tasks",
        )?;

        let rows = stmt.query_map([], |row| row.get(0))?;

        Ok(rows.collect::<Result<Vec<u64>, rusqlite::Error>>()?)
    }
}
