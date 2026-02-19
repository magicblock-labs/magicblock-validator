//! Finalizes V1 program deployment from a buffer account.
//!
//! V1 programs (legacy bpf_loader) are converted to V3 (bpf_loader_upgradeable) format
//! since the ephemeral validator only supports upgradeable loader for V1 programs.

use std::collections::HashSet;

use solana_account::{ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_loader_v3_interface::state::UpgradeableLoaderState;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_sdk_ids::bpf_loader_upgradeable;
use solana_sysvar::rent::Rent;
use solana_transaction_context::TransactionContext;

use super::{
    adjust_authority_lamports, close_buffer_account, get_deploy_slot,
    validate_authority,
};

/// Finalizes a V1 program from a buffer account, converting to V3 (upgradeable loader) format.
///
/// # Why V3?
///
/// The ephemeral validator doesn't support legacy bpf_loader (V1). We convert V1 programs
/// to the upgradeable loader format (V3) which is backwards compatible - the program ID
/// remains the same, and the program_data account is derived from it.
///
/// # Flow
///
/// 1. Reads ELF data from buffer account
/// 2. Creates program_data account with V3 `ProgramData` header + ELF
/// 3. Creates program account with V3 `Program` header (points to program_data)
/// 4. Closes buffer account
///
/// # Account Structure
///
/// ```text
/// program_account (executable):
///   - owner: bpf_loader_upgradeable
///   - data: [Program { programdata_address }]
///
/// program_data_account:
///   - owner: bpf_loader_upgradeable
///   - data: [ProgramData { slot, authority }] + [ELF]
/// ```
///
/// # Lamports Accounting
///
/// Both program and program_data accounts get rent-exempt lamports.
/// The buffer's lamports are returned to the validator authority.
pub(crate) fn process_finalize_v1_program_from_buffer(
    signers: &HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    remote_slot: u64,
    authority: Pubkey,
) -> Result<(), InstructionError> {
    validate_authority(signers, invoke_context)?;

    let ctx = transaction_context.get_current_instruction_context()?;
    let auth_acc = transaction_context.get_account_at_index(
        ctx.get_index_of_instruction_account_in_transaction(0)?,
    )?;
    let prog_acc = transaction_context.get_account_at_index(
        ctx.get_index_of_instruction_account_in_transaction(1)?,
    )?;
    let data_acc = transaction_context.get_account_at_index(
        ctx.get_index_of_instruction_account_in_transaction(2)?,
    )?;
    let buf_acc = transaction_context.get_account_at_index(
        ctx.get_index_of_instruction_account_in_transaction(3)?,
    )?;

    let data_key = *transaction_context.get_key_of_account_at_index(
        ctx.get_index_of_instruction_account_in_transaction(2)?,
    )?;

    let elf_data = buf_acc.borrow().data().to_vec();

    let deploy_slot = get_deploy_slot(invoke_context);

    ic_msg!(
        invoke_context,
        "FinalizeV1: elf_len={} remote_slot={} deploy_slot={}",
        elf_data.len(),
        remote_slot,
        deploy_slot
    );

    // Build V3 program_data account: ProgramData header + ELF
    let program_data_content = {
        let state = UpgradeableLoaderState::ProgramData {
            slot: deploy_slot,
            upgrade_authority_address: Some(authority),
        };
        let mut data = bincode::serialize(&state)
            .map_err(|_| InstructionError::InvalidAccountData)?;
        data.extend_from_slice(&elf_data);
        data
    };

    // Build V3 program account: Program header pointing to program_data
    let program_content = {
        let state = UpgradeableLoaderState::Program {
            programdata_address: data_key,
        };
        bincode::serialize(&state)
            .map_err(|_| InstructionError::InvalidAccountData)?
    };

    // Calculate rent-exempt lamports for both accounts
    let rent = Rent::default();
    let data_lamports = rent.minimum_balance(program_data_content.len());
    let prog_lamports = rent.minimum_balance(program_content.len()).max(1);

    let prog_current = prog_acc.borrow().lamports();
    let data_current = data_acc.borrow().lamports();
    let buf_current = buf_acc.borrow().lamports();
    let lamports_delta = (prog_lamports as i64 - prog_current as i64)
        + (data_lamports as i64 - data_current as i64)
        - buf_current as i64;

    // Set up program_data account
    {
        let mut acc = data_acc.borrow_mut();
        acc.set_lamports(data_lamports);
        acc.set_owner(bpf_loader_upgradeable::id());
        acc.set_executable(false);
        acc.set_data_from_slice(&program_data_content);
        acc.set_remote_slot(remote_slot);
        acc.set_undelegating(false);
    }

    // Set up program account (executable)
    {
        let mut acc = prog_acc.borrow_mut();
        acc.set_lamports(prog_lamports);
        acc.set_owner(bpf_loader_upgradeable::id());
        acc.set_executable(true);
        acc.set_data_from_slice(&program_content);
        acc.set_remote_slot(remote_slot);
        acc.set_undelegating(false);
    }

    // Close buffer account
    close_buffer_account(buf_acc);

    adjust_authority_lamports(auth_acc, lamports_delta)?;
    Ok(())
}
