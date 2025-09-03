use serde::{Deserialize, Serialize};
use solana_program::instruction::Instruction;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleTaskArgs {
    pub task_id: u64,
    pub execution_interval_millis: u64,
    pub n_executions: u64,
    pub instructions: Vec<Instruction>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelTaskArgs {
    pub task_id: u64,
}
