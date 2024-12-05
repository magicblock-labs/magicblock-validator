use std::{collections::HashMap, sync::Arc};

use solana_program_runtime::invoke_context::mock_process_instruction;
use solana_sdk::{
    account::AccountSharedData,
    instruction::{AccountMeta, InstructionError},
    pubkey::Pubkey,
    system_program,
};
use test_tools::validator::PersisterStub;

use self::magicblock_processor::Entrypoint;
use super::*;
use crate::validator;

pub const AUTHORITY_BALANCE: u64 = u64::MAX / 2;
pub fn ensure_started_validator(map: &mut HashMap<Pubkey, AccountSharedData>) {
    validator::generate_validator_authority_if_needed();
    let validator_authority_id = validator::validator_authority_id();
    map.entry(validator_authority_id).or_insert_with(|| {
        AccountSharedData::new(AUTHORITY_BALANCE, 0, &system_program::id())
    });

    let stub = Arc::new(PersisterStub::default());
    init_persister(stub);

    validator::ensure_started_up();
}

pub fn process_instruction(
    instruction_data: &[u8],
    transaction_accounts: Vec<(Pubkey, AccountSharedData)>,
    instruction_accounts: Vec<AccountMeta>,
    expected_result: Result<(), InstructionError>,
) -> Vec<AccountSharedData> {
    mock_process_instruction(
        &crate::id(),
        Vec::new(),
        instruction_data,
        transaction_accounts,
        instruction_accounts,
        expected_result,
        Entrypoint::vm,
        |_invoke_context| {},
        |_invoke_context| {},
    )
}
