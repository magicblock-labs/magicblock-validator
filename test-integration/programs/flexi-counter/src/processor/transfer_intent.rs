use borsh::to_vec;
use ephemeral_rollups_sdk::{
    ephem::{
        ActionCallback, CallHandler, FoldableIntentBuilder,
        MagicIntentBundleBuilder,
    },
    ActionArgs, ShortAccountMeta,
};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, msg, program::invoke,
    program_error::ProgramError, system_instruction,
};

use crate::{
    instruction::FlexiCounterInstruction,
    processor::{
        callback::TRANSFER_CALLBACK_DISCRIMINATOR,
        schedule_intent::ACTOR_ESCROW_INDEX,
    },
};

/// On ER: deducts `amount` lamports from `payer` into the counter PDA as
/// pre-payment, then schedules a CommitAndUndelegate intent with a post-commit
/// action that executes the actual transfer on Base.
///
/// Flow:
/// 1. ER:   `payer → counter_pda` (pre-payment via system CPI)
/// 2. Base: commit action tries to transfer `amount` from escrow → `destination`
///    (returns `TRANSFER_FAIL_CODE` instead if `fail == true`)
/// 3. Base: callback receives `MagicResponse`:
///   - `ok == true`  → no-op, deal closed
///   - `ok == false` → counter_pda refunds `amount` back to `payer`
///
/// Accounts:
/// 0. [signer, write] payer (delegated counter owner)
/// 1. [write]         counter PDA
/// 2. [write]         destination (receives lamports on success)
/// 3. []              system program
/// 4. [write]         magic context
/// 5. []              magic
/// 6. [write]         magic fee vault
pub fn process_create_transfer_intent(
    accounts: &[AccountInfo],
    amount: u64,
    fail: bool,
    compute_units: u32,
) -> ProgramResult {
    msg!("CreateTransferIntent: amount={}, fail={}", amount, fail);

    let [payer, counter_pda, destination, system_program, magic_context, magic_program, magic_fee_vault] =
        accounts
    else {
        msg!(
            "CreateTransferIntent: expected 7 accounts, got {}",
            accounts.len()
        );
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // Pre-payment: move `amount` lamports from payer to counter PDA on ER.
    // These are committed to Base and serve as the refund reserve on failure.
    invoke(
        &system_instruction::transfer(payer.key, counter_pda.key, amount),
        &[payer.clone(), counter_pda.clone(), system_program.clone()],
    )?;

    // Post-commit action: transfer amount from escrow to destination (or fail).
    let action_ix =
        FlexiCounterInstruction::TransferActionHandler { amount, fail };
    let call_handler = CallHandler {
        args: ActionArgs {
            data: to_vec(&action_ix).unwrap(),
            escrow_index: ACTOR_ESCROW_INDEX,
        },
        compute_units,
        escrow_authority: payer.clone(),
        destination_program: crate::id(),
        accounts: vec![
            ShortAccountMeta {
                pubkey: *destination.key,
                is_writable: true,
            },
            system_program.into(),
        ],
    };

    // Callback: receives MagicResponse; refunds counter_pda → payer on failure.
    // `payload` becomes `MagicResponse::data` — we encode amount for refund.
    let callback = ActionCallback {
        destination_program: crate::id(),
        discriminator: TRANSFER_CALLBACK_DISCRIMINATOR.to_vec(),
        payload: amount.to_le_bytes().to_vec(),
        compute_units: 20_000,
        accounts: vec![
            ShortAccountMeta {
                pubkey: *counter_pda.key,
                is_writable: true,
            },
            ShortAccountMeta {
                pubkey: *payer.key,
                is_writable: true,
            },
            system_program.into(),
        ],
    };

    MagicIntentBundleBuilder::new(
        payer.clone(),
        magic_context.clone(),
        magic_program.clone(),
    )
    .magic_fee_vault(magic_fee_vault.clone())
    .commit(std::slice::from_ref(counter_pda))
    .add_post_commit_action_with_callback(call_handler, callback)
    .build_and_invoke()
}
