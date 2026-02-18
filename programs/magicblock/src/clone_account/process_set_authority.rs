//! Updates the authority in a LoaderV4 program header.

use std::collections::HashSet;

use solana_account::{ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_loader_v4_interface::state::LoaderV4State;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_sdk_ids::loader_v4;
use solana_transaction_context::TransactionContext;

use super::validate_authority;

/// Updates the authority field in a LoaderV4 program's header.
///
/// Called after `LoaderV4::Deploy` to set the final authority (from the remote chain).
/// The validator authority is set temporarily during finalize, then replaced with the
/// chain's authority so that the program behaves identically to the remote version.
pub(crate) fn process_set_program_authority(
    signers: &HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    new_authority: Pubkey,
) -> Result<(), InstructionError> {
    validate_authority(signers, invoke_context)?;

    let ctx = transaction_context.get_current_instruction_context()?;
    let idx = ctx.get_index_of_instruction_account_in_transaction(1)?;
    let account = transaction_context.get_account_at_index(idx)?;
    let key = *transaction_context.get_key_of_account_at_index(idx)?;

    // Verify loader v4 ownership
    {
        let acc = account.borrow();
        if acc.owner() != &loader_v4::id() {
            ic_msg!(
                invoke_context,
                "SetProgramAuthority: {} not owned by loader_v4",
                key
            );
            return Err(InstructionError::InvalidAccountOwner);
        }
    }

    // Update authority in header
    let mut acc = account.borrow_mut();
    let data = acc.data();
    let header_size = LoaderV4State::program_data_offset();

    if data.len() < header_size {
        ic_msg!(
            invoke_context,
            "SetProgramAuthority: account data too small"
        );
        return Err(InstructionError::InvalidAccountData);
    }

    // SAFETY: LoaderV4State is POD
    let current: LoaderV4State = unsafe {
        std::ptr::read_unaligned(data.as_ptr() as *const LoaderV4State)
    };

    let new_state = LoaderV4State {
        slot: current.slot,
        authority_address_or_next_version: new_authority,
        status: current.status,
    };

    let header: &[u8] = unsafe {
        std::slice::from_raw_parts(
            (&new_state as *const LoaderV4State) as *const u8,
            header_size,
        )
    };
    acc.data_as_mut_slice()[..header_size].copy_from_slice(header);

    ic_msg!(
        invoke_context,
        "SetProgramAuthority: {} authority -> {}",
        key,
        new_authority
    );
    Ok(())
}
