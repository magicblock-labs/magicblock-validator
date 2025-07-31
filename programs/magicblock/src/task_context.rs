use std::collections::BTreeMap;

use magicblock_magic_program_api::TASK_CONTEXT_SIZE;
use serde::{Deserialize, Serialize};
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    instruction::Instruction,
    pubkey::Pubkey,
};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TaskContext {
    /// Tree containing tasks index by their ID
    pub tasks: BTreeMap<u64, Task>,
}

impl TaskContext {
    pub const SIZE: usize = TASK_CONTEXT_SIZE;
    pub const ZERO: [u8; Self::SIZE] = [0; Self::SIZE];

    pub(crate) fn deserialize(
        data: &AccountSharedData,
    ) -> Result<Self, bincode::Error> {
        if data.data().is_empty() {
            Ok(Self::default())
        } else {
            data.deserialize_data()
        }
    }

    pub(crate) fn add_task(&mut self, task: Task) {
        self.tasks.insert(task.id, task);
    }

    pub(crate) fn remove_task(&mut self, task_id: u64) -> Option<Task> {
        self.tasks.remove(&task_id)
    }

    pub fn get_all_tasks(&self) -> Vec<&Task> {
        self.tasks.values().collect()
    }

    pub fn get_task(&self, task_id: u64) -> Option<&Task> {
        self.tasks.get(&task_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Task {
    /// Unique identifier for this task
    pub id: u64,
    /// Unsigned instructions to execute when triggered
    pub instructions: Vec<Instruction>,
    /// Authority that can modify or cancel this task
    pub authority: Pubkey,
    /// Period of the task in milliseconds
    pub period_millis: i64,
    /// Number of times this task will be executed
    pub n_executions: u64,
}

impl Task {
    pub fn new(
        id: u64,
        instructions: Vec<Instruction>,
        authority: Pubkey,
        period_millis: i64,
        n_executions: u64,
    ) -> Self {
        Self {
            id,
            instructions,
            authority,
            period_millis,
            n_executions,
        }
    }
}
