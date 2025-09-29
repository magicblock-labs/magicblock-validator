use std::cell::RefCell;

use magicblock_magic_program_api::TASK_CONTEXT_SIZE;
use serde::{Deserialize, Serialize};
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    instruction::{Instruction, InstructionError},
    pubkey::Pubkey,
};

pub const MIN_EXECUTION_INTERVAL: u64 = 10;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskRequest {
    Schedule(ScheduleTaskRequest),
    Cancel(CancelTaskRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduleTaskRequest {
    /// Unique identifier for this task
    pub id: u64,
    /// Unsigned instructions to execute when triggered
    pub instructions: Vec<Instruction>,
    /// Authority that can modify or cancel this task
    pub authority: Pubkey,
    /// How frequently the task should be executed, in milliseconds
    pub execution_interval_millis: u64,
    /// Number of times this task will be executed
    pub iterations: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CancelTaskRequest {
    /// Unique identifier for the task to cancel
    pub task_id: u64,
    /// Authority that can cancel this task
    pub authority: Pubkey,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TaskContext {
    /// List of requests
    pub requests: Vec<TaskRequest>,
}

impl TaskContext {
    pub const SIZE: usize = TASK_CONTEXT_SIZE;
    pub const ZERO: [u8; Self::SIZE] = [0; Self::SIZE];

    pub fn add_request(
        context_acc: &RefCell<AccountSharedData>,
        request: TaskRequest,
    ) -> Result<(), InstructionError> {
        Self::update_context(context_acc, |context| {
            context.requests.push(request)
        })
    }

    pub fn clear_requests(
        context_acc: &RefCell<AccountSharedData>,
    ) -> Result<(), InstructionError> {
        Self::update_context(context_acc, |context| context.requests.clear())
    }

    fn update_context(
        context_acc: &RefCell<AccountSharedData>,
        update_fn: impl FnOnce(&mut TaskContext),
    ) -> Result<(), InstructionError> {
        let mut context = Self::deserialize(&context_acc.borrow())
            .map_err(|_| InstructionError::GenericError)?;
        update_fn(&mut context);

        let serialized_data = bincode::serialize(&context)
            .map_err(|_| InstructionError::InvalidAccountData)?;
        let mut context_data = context_acc.borrow_mut();
        context_data.resize(serialized_data.len(), 0);
        context_data.set_data_from_slice(&serialized_data);
        Ok(())
    }

    pub(crate) fn deserialize(
        data: &AccountSharedData,
    ) -> Result<Self, bincode::Error> {
        if data.data().is_empty() {
            Ok(Self::default())
        } else {
            data.deserialize_data()
        }
    }
}

// Keep the old Task struct for backward compatibility and database storage
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CrankTask {
    /// Unique identifier for this task
    pub id: u64,
    /// Unsigned instructions to execute when triggered
    pub instructions: Vec<Instruction>,
    /// Authority that can modify or cancel this task
    pub authority: Pubkey,
    /// How frequently the task should be executed, in milliseconds
    pub execution_interval_millis: u64,
    /// Number of times this task will be executed
    pub iterations: u64,
}

impl CrankTask {
    pub fn new(
        id: u64,
        instructions: Vec<Instruction>,
        authority: Pubkey,
        execution_interval_millis: u64,
        iterations: u64,
    ) -> Self {
        Self {
            id,
            instructions,
            authority,
            execution_interval_millis,
            iterations,
        }
    }
}

impl From<&ScheduleTaskRequest> for CrankTask {
    fn from(request: &ScheduleTaskRequest) -> Self {
        Self {
            id: request.id,
            instructions: request.instructions.clone(),
            authority: request.authority,
            execution_interval_millis: request.execution_interval_millis,
            iterations: request.iterations,
        }
    }
}
