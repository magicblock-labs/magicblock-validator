use std::path::Path;

use chrono::Utc;
use log::*;
use r2d2::Pool;
use r2d2_sqlite::{
    rusqlite::{self, params},
    SqliteConnectionManager,
};
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
    /// Period of the task in milliseconds
    pub period_millis: i64,
    /// Number of times this task still needs to be executed.
    pub executions_left: u64,
    /// Timestamp of the next execution of this task in milliseconds
    pub next_execution_millis: i64,
}

#[derive(Clone)]
pub struct SchedulerDatabase {
    pool: Pool<SqliteConnectionManager>,
}

impl SchedulerDatabase {
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, TaskSchedulerError> {
        let manager = SqliteConnectionManager::file(path);
        let pool = Pool::new(manager)?;

        let conn = pool.get()?;

        // Create tables
        conn.execute(
            "CREATE TABLE IF NOT EXISTS tasks (
                id INTEGER PRIMARY KEY,
                instructions BLOB NOT NULL,
                authority TEXT NOT NULL,
                period_millis INTEGER NOT NULL,
                executions_left INTEGER NOT NULL,
                next_execution_millis INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_next_execution ON tasks(next_execution_millis)",
            [],
        )?;

        Ok(Self { pool })
    }

    pub fn insert_task(&self, task: &DbTask) -> Result<(), TaskSchedulerError> {
        let conn = self.pool.get()?;
        let instructions_bin = bincode::serialize(&task.instructions)?;
        let authority_str = task.authority.to_string();
        let now = Utc::now().timestamp_millis();

        conn.execute(
            "INSERT OR REPLACE INTO tasks 
             (id, instructions, authority, period_millis, executions_left, next_execution_millis, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                task.id,
                instructions_bin,
                authority_str,
                task.period_millis,
                task.executions_left,
                task.next_execution_millis,
                now,
                now,
            ],
        )?;

        debug!("Inserted task {} into database", task.id);
        Ok(())
    }

    pub fn update_task_after_execution(
        &self,
        task_id: u64,
        next_execution: i64,
    ) -> Result<(), TaskSchedulerError> {
        let now = Utc::now().timestamp_millis();

        let conn = self.pool.get()?;
        conn.execute(
            "UPDATE tasks 
             SET executions_left = executions_left - 1, 
                 next_execution_millis = ?,
                 updated_at = ?
             WHERE id = ?",
            params![next_execution, now, task_id],
        )?;

        debug!("Updated task {} after execution", task_id);
        Ok(())
    }

    pub fn unschedule_task(
        &self,
        task_id: u64,
    ) -> Result<(), TaskSchedulerError> {
        let conn = self.pool.get()?;
        conn.execute(
            "UPDATE tasks SET next_execution_millis = 0, executions_left = 0 WHERE id = ?",
            [task_id],
        )?;
        debug!("Unscheduled task {}", task_id);
        Ok(())
    }

    pub fn remove_task(&self, task_id: u64) -> Result<(), TaskSchedulerError> {
        let conn = self.pool.get()?;
        conn.execute("DELETE FROM tasks WHERE id = ?", [task_id])?;
        debug!("Removed task {} from database", task_id);
        Ok(())
    }

    pub fn get_task(
        &self,
        task_id: u64,
    ) -> Result<Option<DbTask>, TaskSchedulerError> {
        let conn = self.pool.get()?;
        let mut stmt = conn.prepare(
            "SELECT id, instructions, authority, period_millis, executions_left, next_execution_millis
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
                period_millis: row.get(3)?,
                executions_left: row.get(4)?,
                next_execution_millis: row.get(5)?,
            })
        })?;

        Ok(rows.next().transpose()?)
    }

    pub fn get_task_ids(&self) -> Result<Vec<u64>, TaskSchedulerError> {
        let conn = self.pool.get()?;
        let mut stmt = conn.prepare(
            "SELECT id 
             FROM tasks",
        )?;

        let rows = stmt.query_map([], |row| row.get(0))?;

        Ok(rows.collect::<Result<Vec<u64>, rusqlite::Error>>()?)
    }

    pub fn get_executable_tasks(
        &self,
        current_time: i64,
    ) -> Result<Vec<DbTask>, TaskSchedulerError> {
        let conn = self.pool.get()?;
        let mut stmt = conn.prepare(
            "SELECT id, instructions, authority, period_millis, executions_left, next_execution_millis
             FROM tasks 
             WHERE next_execution_millis <= ? AND executions_left > 0
             ORDER BY next_execution_millis ASC"
        )?;

        let mut tasks = Vec::new();
        let rows = stmt.query_map([current_time], |row| {
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
                period_millis: row.get(3)?,
                executions_left: row.get(4)?,
                next_execution_millis: row.get(5)?,
            })
        })?;

        for row in rows {
            tasks.push(row?);
        }

        Ok(tasks)
    }
}
