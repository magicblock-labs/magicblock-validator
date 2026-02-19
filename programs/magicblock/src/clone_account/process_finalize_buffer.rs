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

use super::{
    adjust_authority_lamports, close_buffer_account, get_deploy_slot,
    loader_v4_state_to_bytes, validate_authority,
};
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
/// We use `current_slot - 5` for the deploy slot to bypass LoaderV4's cooldown mechanism.
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
    remote_slot: u64,
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

    let buf_data = buf_acc.borrow().data().to_vec();
    let buf_lamports = buf_acc.borrow().lamports();
    let prog_current_lamports = prog_acc.borrow().lamports();

    let deploy_slot = get_deploy_slot(invoke_context);

    // Build LoaderV4 account data: header + ELF
    let state = LoaderV4State {
        slot: deploy_slot,
        authority_address_or_next_version: validator_authority_id(),
        status: LoaderV4Status::Retracted, // Deploy instruction will activate it
    };
    let program_data = build_loader_v4_data(&state, &buf_data);

    ic_msg!(
        invoke_context,
        "FinalizeProgram: elf_len={} remote_slot={} deploy_slot={}",
        buf_data.len(),
        remote_slot,
        deploy_slot
    );

    // Calculate rent-exempt lamports for full program account
    let prog_lamports = Rent::default().minimum_balance(program_data.len());
    let lamports_delta = prog_lamports as i64
        - prog_current_lamports as i64
        - buf_lamports as i64;

    // Set up program account
    {
        let mut prog = prog_acc.borrow_mut();
        prog.set_lamports(prog_lamports);
        prog.set_owner(loader_v4::id());
        prog.set_executable(true);
        prog.set_data_from_slice(&program_data);
        prog.set_remote_slot(remote_slot);
        prog.set_undelegating(false);
    }

    // Close buffer account
    close_buffer_account(buf_acc);

    adjust_authority_lamports(auth_acc, lamports_delta)?;
    Ok(())
}

/// Builds LoaderV4 account data by prepending the state header to the program data.
fn build_loader_v4_data(state: &LoaderV4State, program_data: &[u8]) -> Vec<u8> {
    let header_size = LoaderV4State::program_data_offset();
    let mut data = Vec::with_capacity(header_size + program_data.len());
    data.extend_from_slice(loader_v4_state_to_bytes(state));
    data.extend_from_slice(program_data);
    data
}
