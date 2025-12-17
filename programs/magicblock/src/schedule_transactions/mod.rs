mod process_accept_scheduled_commits;
mod process_schedule_base_intent;
mod process_schedule_commit;
#[cfg(test)]
mod process_schedule_commit_tests;
mod process_scheduled_commit_sent;
pub(crate) mod transaction_scheduler;

use magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY;
pub(crate) use process_accept_scheduled_commits::*;
pub(crate) use process_schedule_base_intent::*;
pub(crate) use process_schedule_commit::*;
pub use process_scheduled_commit_sent::{
    process_scheduled_commit_sent, register_scheduled_commit_sent, SentCommit,
};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_sdk::instruction::InstructionError;

use crate::utils::accounts::get_instruction_pubkey_with_idx;

pub fn check_magic_context_id(
    invoke_context: &InvokeContext,
    idx: u16,
) -> Result<(), InstructionError> {
    let provided_magic_context = get_instruction_pubkey_with_idx(
        invoke_context.transaction_context,
        idx,
    )?;
    if !provided_magic_context.eq(&MAGIC_CONTEXT_PUBKEY) {
        ic_msg!(
            invoke_context,
            "ERR: invalid magic context account {}",
            provided_magic_context
        );
        return Err(InstructionError::MissingAccount);
    }

    Ok(())
}

// Helper to remap SPL Token ATAs to eATAs for delegated accounts.
// This keeps ScheduleCommit and ScheduleBaseIntent paths consistent.
use magicblock_core::token_programs::{
    derive_ata, derive_eata, SPL_TOKEN_PROGRAM_ID,
};
use solana_sdk::{
    account::{AccountSharedData, ReadableAccount},
    pubkey::Pubkey,
};

pub(crate) fn remap_ata_to_eata_if_delegated(
    account: &AccountSharedData,
    pubkey: &Pubkey,
) -> Pubkey {
    if account.delegated() && account.owner() == &SPL_TOKEN_PROGRAM_ID {
        let data = account.data();
        if data.len() >= 64 {
            // SAFETY: slices validated by len check; fall back to original key on any parse error
            if let (Ok(mint_arr), Ok(owner_arr)) = (
                <[u8; 32]>::try_from(&data[0..32]),
                <[u8; 32]>::try_from(&data[32..64]),
            ) {
                let mint = Pubkey::new_from_array(mint_arr);
                let wallet_owner = Pubkey::new_from_array(owner_arr);
                let ata = derive_ata(&wallet_owner, &mint);
                if ata == *pubkey {
                    return derive_eata(&wallet_owner, &mint);
                }
            }
        }
    }

    *pubkey
}
