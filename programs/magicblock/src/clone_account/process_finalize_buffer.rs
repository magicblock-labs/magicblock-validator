//! Finalizes V4 program deployment from a buffer account.
//!
//! This is part of the program cloning flow for LoaderV4 programs (V2/V3/V4 on remote chain).

use std::collections::HashSet;

use solana_account::{ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_loader_v4_interface::state::{LoaderV4State, LoaderV4Status};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_sdk_ids::loader_v4;
use solana_sysvar::rent::Rent;
use solana_transaction_context::TransactionContext;

use super::{adjust_authority_lamports, validate_authority};
use crate::validator::validator_authority_id;

/// Finalizes a LoaderV4 program from a buffer account.
///
/// # Flow
///
/// 1. Reads ELF data from buffer account
/// 2. Prepends LoaderV4State header with Retracted status
/// 3. Sets program account as executable with proper lamports
/// 4. Closes buffer account
///
/// After this instruction, the caller must invoke `LoaderV4::Deploy` then `SetProgramAuthority`.
///
/// # Slot Trick
///
/// We use `slot - 5` for the deploy slot to bypass LoaderV4's cooldown mechanism.
/// LoaderV4 requires programs to wait N slots after deployment before certain operations.
/// By setting the slot to 5 slots ago, we simulate that the program was deployed earlier.
///
/// # Lamports Accounting
///
/// The program account gets rent-exempt lamports for (header + ELF).
/// The buffer's lamports are returned to the validator authority.
pub(crate) fn process_finalize_program_from_buffer(
    signers: &HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    slot: u64,
) -> Result<(), InstructionError> {
    validate_authority(signers, invoke_context)?;

    let ctx = transaction_context.get_current_instruction_context()?;
    let auth_acc = transaction_context.get_account_at_index(
        ctx.get_index_of_instruction_account_in_transaction(0)?,
    )?;
    let prog_acc = transaction_context.get_account_at_index(
        ctx.get_index_of_instruction_account_in_transaction(1)?,
    )?;
    let buf_acc = transaction_context.get_account_at_index(
        ctx.get_index_of_instruction_account_in_transaction(2)?,
    )?;

    let prog_key = *transaction_context.get_key_of_account_at_index(
        ctx.get_index_of_instruction_account_in_transaction(1)?,
    )?;
    let buf_key = *transaction_context.get_key_of_account_at_index(
        ctx.get_index_of_instruction_account_in_transaction(2)?,
    )?;

    let buf_data = buf_acc.borrow().data().to_vec();
    let buf_lamports = buf_acc.borrow().lamports();
    let prog_current_lamports = prog_acc.borrow().lamports();

    ic_msg!(invoke_context, "FinalizeV4: prog={} buf={} len={}", prog_key, buf_key, buf_data.len());

    // Build LoaderV4 account data: header + ELF
    let deploy_slot = slot.saturating_sub(5).max(1); // Bypass cooldown
    let state = LoaderV4State {
        slot: deploy_slot,
        authority_address_or_next_version: validator_authority_id(),
        status: LoaderV4Status::Retracted, // Deploy instruction will activate it
    };
    let program_data = build_loader_v4_data(&state, &buf_data);

    // Calculate rent-exempt lamports for full program account
    let prog_lamports = Rent::default().minimum_balance(program_data.len());
    let lamports_delta = prog_lamports as i64 - prog_current_lamports as i64 - buf_lamports as i64;

    // Set up program account
    {
        let mut prog = prog_acc.borrow_mut();
        prog.set_lamports(prog_lamports);
        prog.set_owner(loader_v4::id());
        prog.set_executable(true);
        prog.set_data_from_slice(&program_data);
        prog.set_remote_slot(slot);
        prog.set_undelegating(false);
    }

    // Close buffer account
    {
        let mut buf = buf_acc.borrow_mut();
        buf.set_lamports(0);
        buf.set_data_from_slice(&[]);
        buf.set_executable(false);
        buf.set_owner(Pubkey::default());
    }

    adjust_authority_lamports(auth_acc, lamports_delta)?;
    ic_msg!(invoke_context, "FinalizeV4: finalized {}, closed {}", prog_key, buf_key);
    Ok(())
}

/// Builds LoaderV4 account data by prepending the state header to the program data.
fn build_loader_v4_data(state: &LoaderV4State, program_data: &[u8]) -> Vec<u8> {
    let header_size = LoaderV4State::program_data_offset();
    let mut data = Vec::with_capacity(header_size + program_data.len());
    // SAFETY: LoaderV4State is POD with no uninitialized padding
    let header: &[u8] = unsafe {
        std::slice::from_raw_parts((state as *const LoaderV4State) as *const u8, header_size)
    };
    data.extend_from_slice(header);
    data.extend_from_slice(program_data);
    data
}
