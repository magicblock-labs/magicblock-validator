use solana_sdk::{
    inner_instruction::InnerInstructions,
    transaction::Result,
    transaction_context::{TransactionAccount, TransactionReturnData},
};
use solana_svm::transaction_processor::TransactionLogMessages;

pub struct TransactionSimulationResult {
    pub result: Result<()>,
    pub logs: TransactionLogMessages,
    pub post_simulation_accounts: Vec<TransactionAccount>,
    pub units_consumed: u64,
    pub return_data: Option<TransactionReturnData>,
    pub inner_instructions: Option<Vec<InnerInstructions>>,
}
