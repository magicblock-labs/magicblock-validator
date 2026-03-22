use magicblock_magic_program_api::response::MagicResponse;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, msg, program::invoke,
    program_error::ProgramError, system_instruction,
};

/// Discriminator prefix for the transfer callback instruction.
/// Checked before borsh parsing since the callback carries bincode-encoded
/// `MagicResponse` rather than a borsh instruction.
pub const TRANSFER_CALLBACK_DISCRIMINATOR: &[u8] =
    &[0xFE, 0xCA, 0xCB, 0x01, 0x00, 0x00, 0x00, 0x00];

/// Custom error code returned when the transfer action intentionally fails,
/// used to exercise the callback refund path in tests.
pub const TRANSFER_FAIL_CODE: u32 = 0xFA11;

/// Post-commit action: transfers `amount` lamports from the payer's escrow to
/// `destination` on Base. If `fail` is set the instruction returns an error
/// so the magic program fires the callback with `ok = false`.
///
/// Accounts dispatched by the magic program:
/// 0. [write] destination
/// 1. []      system program
/// 2. []      source program (auto-prepended, validated)
/// 3. []      escrow authority (auto)
/// 4. [signer, write] escrow account (ephemeral_balance_pda)
pub fn process_transfer_action_handler(
    accounts: &[AccountInfo],
    amount: u64,
    fail: bool,
) -> ProgramResult {
    msg!("TransferActionHandler: amount={}, fail={}", amount, fail);

    let [destination, system_program, source_program, _, escrow_account] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if source_program.key != &crate::id() {
        msg!(
            "TransferActionHandler: invalid source program {}",
            source_program.key
        );
        return Err(ProgramError::IncorrectProgramId);
    }
    if !escrow_account.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    if fail {
        msg!("TransferActionHandler: intentionally failing");
        return Err(ProgramError::Custom(TRANSFER_FAIL_CODE));
    }

    invoke(
        &system_instruction::transfer(
            escrow_account.key,
            destination.key,
            amount,
        ),
        &[
            escrow_account.clone(),
            destination.clone(),
            system_program.clone(),
        ],
    )
}

/// Callback fired by the magic program after the transfer action completes.
///
/// The sole argument is `MagicResponse { ok, data, error }` (bincode-encoded
/// after the discriminator prefix). `data` carries the 8-byte little-endian
/// `u64` amount set as the `ActionCallback::payload` at intent creation time.
///
/// On success (`ok == true`): no-op — destination has the lamports; the
/// counter PDA retains the pre-payment as the cost of the deal.
///
/// On failure (`ok == false`): moves `amount` lamports from the counter PDA
/// back to the payer on Base (refund). The counter PDA can be written because
/// at callback time the account has already been undelegated and is owned by
/// the flexi-counter program again.
///
/// Accounts:
/// 0. [signer, readonly] validator authority (auto-prepended by magic program)
/// 1. [write] counter PDA (pre-payment source / refund sender)
/// 2. [write] payer (refund recipient on failure)
/// 3. []      system program
pub fn process_transfer_callback(
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    msg!("TransferCallback");

    let [_validator_authority, counter_pda, payer, _system_program] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let response: MagicResponse = bincode::deserialize(data)
        .map_err(|_| ProgramError::InvalidInstructionData)?;

    msg!(
        "TransferCallback: ok={}, error={}",
        response.ok,
        response.error
    );

    if !response.ok {
        let amount = u64::from_le_bytes(
            response
                .data
                .try_into()
                .map_err(|_| ProgramError::InvalidInstructionData)?,
        );
        msg!("TransferCallback: refunding {} lamports to payer", amount);
        **counter_pda.try_borrow_mut_lamports()? -= amount;
        **payer.try_borrow_mut_lamports()? += amount;
    }

    Ok(())
}
