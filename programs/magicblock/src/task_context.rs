use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
};

use serde::{Deserialize, Serialize};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
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
    /// Tree containing the IDs of scheduled tasks.
    pub scheduled_tasks: BTreeSet<u64>,
    /// Tree containing task requests indexed by their ID.
    /// Schedule and cancels requests have the same ID and are stored in the same map.
    pub requests: BTreeMap<u64, TaskRequest>,
}

impl TaskContext {
    pub fn schedule_task(
        invoke_context: &InvokeContext,
        context_account: &RefCell<AccountSharedData>,
        request: ScheduleTaskRequest,
    ) -> Result<(), InstructionError> {
        Self::update_task_context(invoke_context, context_account, |context| {
            context.add_schedule_request(request);
        })
    }

    pub fn cancel_task(
        invoke_context: &InvokeContext,
        context_account: &RefCell<AccountSharedData>,
        request: CancelTaskRequest,
    ) -> Result<(), InstructionError> {
        Self::update_task_context(invoke_context, context_account, |context| {
            context.add_cancel_request(request);
        })
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

    pub(crate) fn add_schedule_request(
        &mut self,
        request: ScheduleTaskRequest,
    ) {
        self.scheduled_tasks.insert(request.id);
        self.requests
            .insert(request.id, TaskRequest::Schedule(request));
    }

    pub(crate) fn add_cancel_request(&mut self, request: CancelTaskRequest) {
        self.requests
            .insert(request.task_id, TaskRequest::Cancel(request));
    }

    pub fn remove_request(&mut self, request_id: u64) -> Option<TaskRequest> {
        match self.requests.remove(&request_id) {
            Some(TaskRequest::Cancel(request)) => {
                self.scheduled_tasks.remove(&request_id);
                Some(TaskRequest::Cancel(request))
            }
            _ => None,
        }
    }

    pub fn get_all_requests(&self) -> Vec<TaskRequest> {
        self.requests.values().cloned().collect()
    }

    pub fn get_request(&self, request_id: u64) -> Option<&TaskRequest> {
        self.requests.get(&request_id)
    }

    pub fn get_schedule_requests(&self) -> Vec<&ScheduleTaskRequest> {
        self.requests
            .values()
            .filter_map(|req| match req {
                TaskRequest::Schedule(schedule_req) => Some(schedule_req),
                TaskRequest::Cancel(_) => None,
            })
            .collect()
    }

    pub fn get_cancel_requests(&self) -> Vec<&CancelTaskRequest> {
        self.requests
            .values()
            .filter_map(|req| match req {
                TaskRequest::Schedule(_) => None,
                TaskRequest::Cancel(cancel_req) => Some(cancel_req),
            })
            .collect()
    }

    fn update_task_context<F>(
        invoke_context: &InvokeContext,
        context_account: &RefCell<AccountSharedData>,
        update_fn: F,
    ) -> Result<(), InstructionError>
    where
        F: FnOnce(&mut TaskContext),
    {
        let context_data = &mut context_account.borrow_mut();
        let mut context =
            TaskContext::deserialize(context_data).map_err(|err| {
                ic_msg!(
                    invoke_context,
                    "Failed to deserialize TaskContext: {}",
                    err
                );
                InstructionError::GenericError
            })?;
        update_fn(&mut context);
        let serialized = bincode::serialize(&context).map_err(|err| {
            ic_msg!(invoke_context, "Failed to serialize TaskContext: {}", err);
            InstructionError::GenericError
        })?;
        context_data.resize(serialized.len(), 0);
        context_data.set_data_from_slice(&serialized);
        Ok(())
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
