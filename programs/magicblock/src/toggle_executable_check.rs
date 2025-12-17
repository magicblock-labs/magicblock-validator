use std::collections::HashSet;

use magicblock_magic_program_api::Pubkey;
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;

use crate::{
    utils::accounts::get_instruction_pubkey_with_idx,
    validator::validator_authority_id,
};

/// Enables or disables the executable flag checks for the provided `invoke_context`.
/// NOTE: this applies globally and once removed will allow modifying executable data
///       for all transactions that follow until it is re-enabled.
pub(crate) fn process_toggle_executable_check(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    enable: bool,
) -> Result<(), InstructionError> {
    const VALIDATOR_AUTHORITY_IDX: u16 = 0;

    // Check that the validator authority (first account) is correct and signer
    let provided_validator_auth = get_instruction_pubkey_with_idx(
        invoke_context.transaction_context,
        VALIDATOR_AUTHORITY_IDX,
    )?;
    let validator_auth = validator_authority_id();
    if !provided_validator_auth.eq(&validator_auth) {
        ic_msg!(
             invoke_context,
             "ToggleExecutableCheck: invalid validator authority {}, should be {}",
             provided_validator_auth,
             validator_auth
         );
        return Err(InstructionError::InvalidArgument);
    }
    if !signers.contains(&validator_auth) {
        ic_msg!(
            invoke_context,
            "ToggleExecutableCheck:  validator authority pubkey {} not in signers",
            validator_auth
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    invoke_context
        .transaction_context
        .set_remove_accounts_executable_flag_checks(!enable);
    Ok(())
}
