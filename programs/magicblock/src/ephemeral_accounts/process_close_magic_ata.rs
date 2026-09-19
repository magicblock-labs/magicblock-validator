use magicblock_core::token_programs::try_get_magic_ata_info;
use solana_account::{AccountMode, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_transaction_context::transaction::TransactionContext;

use crate::utils::{
    account_actions::set_account_mode,
    accounts::{
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
    },
};

const OWNER_IDX: u16 = 0;
const ATA_IDX: u16 = 1;

pub(crate) fn process_close_magic_ata(
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
) -> Result<(), InstructionError> {
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    if !ix_ctx.is_instruction_account_signer(OWNER_IDX)? {
        return Err(InstructionError::MissingRequiredSignature);
    }

    let owner =
        *get_instruction_pubkey_with_idx(transaction_context, OWNER_IDX)?;
    let ata_pubkey =
        *get_instruction_pubkey_with_idx(transaction_context, ATA_IDX)?;

    let ata = get_instruction_account_with_idx(transaction_context, ATA_IDX)?;

    // No-op unless the account is this owner's drained Magic ATA, so
    // withdrawal flows can append this instruction unconditionally.
    let closeable = {
        let account = ata.borrow()?;
        try_get_magic_ata_info(&ata_pubkey, &account)
            .is_some_and(|info| info.wallet_owner == owner && info.amount == 0)
    };
    if !closeable {
        return Ok(());
    }

    let mut acc = ata.borrow_mut()?;
    acc.set_lamports(0);
    acc.set_owner(Pubkey::default());
    acc.resize(0, 0);
    set_account_mode(invoke_context, &mut acc, AccountMode::Closed)?;

    ic_msg!(
        invoke_context,
        "Closed Magic ATA {} for owner {}",
        ata_pubkey,
        owner
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use magicblock_core::token_programs::{
        MAGIC_ATA_CLOSE_AUTHORITY, TOKEN_PROGRAM_ID, derive_ata,
    };
    use magicblock_magic_program_api::instruction::MagicBlockInstruction;
    use solana_account::{AccountBuilder, AccountSharedData, ReadableAccount};
    use solana_instruction::{AccountMeta, Instruction};
    use solana_program::{program_option::COption, program_pack::Pack};
    use solana_sdk_ids::system_program;
    use spl_token::state::{
        Account as SplAccount, AccountState as SplAccountState,
    };

    use super::*;
    use crate::test_utils::process_instruction;

    fn magic_ata_account(
        wallet_owner: Pubkey,
        mint: Pubkey,
        amount: u64,
    ) -> AccountSharedData {
        let token_account = SplAccount {
            mint,
            owner: wallet_owner,
            amount,
            delegate: COption::None,
            state: SplAccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::Some(MAGIC_ATA_CLOSE_AUTHORITY),
        };
        let mut account =
            AccountSharedData::new(0, SplAccount::LEN, &TOKEN_PROGRAM_ID);
        SplAccount::pack(token_account, account.data_as_mut_slice()).unwrap();
        AccountBuilder::from(account)
            .mode(AccountMode::Magic)
            .build()
    }

    fn close_ix(owner: Pubkey, ata: Pubkey) -> Instruction {
        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::CloseMagicAta,
            vec![
                AccountMeta::new_readonly(owner, true),
                AccountMeta::new(ata, false),
            ],
        )
    }

    #[test]
    fn close_magic_ata_removes_drained_account() {
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata = derive_ata(&wallet_owner, &mint);

        let ix = close_ix(wallet_owner, ata);
        let accounts = process_instruction(
            &ix.data,
            vec![
                (
                    wallet_owner,
                    AccountSharedData::new(1_000_000, 0, &system_program::id()),
                ),
                (ata, magic_ata_account(wallet_owner, mint, 0)),
            ],
            ix.accounts,
            Ok(()),
        );

        let ata_after = &accounts[1];
        assert_eq!(ata_after.lamports(), 0);
        assert_eq!(ata_after.owner(), &Pubkey::default());
        assert!(ata_after.data().is_empty());
        assert!(ata_after.is(AccountMode::Closed));
    }

    #[test]
    fn close_magic_ata_noops_when_funded() {
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata = derive_ata(&wallet_owner, &mint);
        let funded = magic_ata_account(wallet_owner, mint, 5);

        let ix = close_ix(wallet_owner, ata);
        let accounts = process_instruction(
            &ix.data,
            vec![
                (
                    wallet_owner,
                    AccountSharedData::new(1_000_000, 0, &system_program::id()),
                ),
                (ata, funded.clone()),
            ],
            ix.accounts,
            Ok(()),
        );

        assert_eq!(accounts[1].data(), funded.data());
        assert!(accounts[1].is(AccountMode::Magic));
    }

    #[test]
    fn close_magic_ata_noops_for_other_signer() {
        let wallet_owner = Pubkey::new_unique();
        let other = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata = derive_ata(&wallet_owner, &mint);
        let drained = magic_ata_account(wallet_owner, mint, 0);

        let ix = close_ix(other, ata);
        let accounts = process_instruction(
            &ix.data,
            vec![
                (
                    other,
                    AccountSharedData::new(1_000_000, 0, &system_program::id()),
                ),
                (ata, drained.clone()),
            ],
            ix.accounts,
            Ok(()),
        );

        assert_eq!(accounts[1].data(), drained.data());
        assert!(accounts[1].is(AccountMode::Magic));
    }

    #[test]
    fn close_magic_ata_noops_for_missing_account() {
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata = derive_ata(&wallet_owner, &mint);

        let ix = close_ix(wallet_owner, ata);
        process_instruction(
            &ix.data,
            vec![
                (
                    wallet_owner,
                    AccountSharedData::new(1_000_000, 0, &system_program::id()),
                ),
                (ata, AccountSharedData::new(0, 0, &system_program::id())),
            ],
            ix.accounts,
            Ok(()),
        );
    }

    #[test]
    fn close_magic_ata_requires_owner_signature() {
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata = derive_ata(&wallet_owner, &mint);

        let mut ix = close_ix(wallet_owner, ata);
        ix.accounts[0].is_signer = false;
        process_instruction(
            &ix.data,
            vec![
                (
                    wallet_owner,
                    AccountSharedData::new(1_000_000, 0, &system_program::id()),
                ),
                (ata, magic_ata_account(wallet_owner, mint, 0)),
            ],
            ix.accounts,
            Err(InstructionError::MissingRequiredSignature),
        );
    }
}
