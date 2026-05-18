use std::collections::HashSet;

use solana_account::{ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_transaction_context::TransactionContext;

use super::{
    adjust_authority_lamports, validate_and_get_index, validate_authority,
};
use crate::errors::MagicBlockProgramError;

pub(crate) fn process_evict_account(
    signers: &HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    pubkey: Pubkey,
) -> Result<(), InstructionError> {
    validate_authority(signers, invoke_context)?;

    let tx_idx = validate_and_get_index(
        transaction_context,
        1,
        &pubkey,
        "EvictAccount",
        invoke_context,
    )?;
    let mut acc = transaction_context.accounts().try_borrow_mut(tx_idx)?;

    {
        if acc.delegated() || acc.undelegating() {
            ic_msg!(
                invoke_context,
                "EvictAccount: account {} is delegated={} \
                 undelegating={}, rejecting",
                pubkey,
                acc.delegated(),
                acc.undelegating()
            );
            return Err(MagicBlockProgramError::AccountIsDelegated.into());
        }
    }

    let evicted_lamports = acc.lamports();
    if evicted_lamports > 0 {
        let delta = i64::try_from(evicted_lamports)
            .map_err(|_| InstructionError::ArithmeticOverflow)?;
        let ctx = transaction_context.get_current_instruction_context()?;
        let mut auth_acc = transaction_context.accounts().try_borrow_mut(
            ctx.get_index_of_instruction_account_in_transaction(0)?,
        )?;
        adjust_authority_lamports(&mut auth_acc, -delta)?;
    }

    {
        acc.set_lamports(0);
        acc.set_owner(Pubkey::default());
        acc.resize(0, 0);
        acc.set_delegated(false);
        acc.set_confined(false);
        // Setting ephemeral=true with owner=Pubkey::default()
        // triggers AccountsDb::upsert to atomically remove the
        // account from the LMDB index during commit.
        acc.set_ephemeral(true);
    }

    ic_msg!(invoke_context, "EvictAccount: evicted '{}'", pubkey);
    Ok(())
}
