use magicblock_core::token_programs::try_get_rent_pending_ata_info;
use solana_account::WritableAccount;
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_transaction_context::TransactionContext;

use crate::utils::accounts::{
    get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
};

const OWNER_IDX: u16 = 0;
const ATA_IDX: u16 = 1;

pub(crate) fn process_close_rent_pending_ata(
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
    let ata_shared = ata.to_account_shared_data()?;

    // No-op unless the account is this owner's drained rent-pending ATA, so
    // withdrawal flows can append this instruction unconditionally.
    let closeable = try_get_rent_pending_ata_info(&ata_pubkey, &ata_shared)
        .is_some_and(|info| info.wallet_owner == owner && info.amount == 0);
    if !closeable {
        return Ok(());
    }

    let mut acc = ata.borrow_mut()?;
    acc.set_lamports(0);
    acc.set_owner(Pubkey::default());
    acc.resize(0, 0);
    acc.set_delegated(false);
    acc.set_confined(false);
    acc.set_undelegating(false);
    // Setting ephemeral=true with owner=Pubkey::default() triggers
    // AccountsDb::upsert to remove the account during commit.
    acc.set_ephemeral(true);

    ic_msg!(
        invoke_context,
        "Closed rent-pending ATA {} for owner {}",
        ata_pubkey,
        owner
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use magicblock_core::token_programs::{
        derive_ata, RENT_PENDING_ATA_CLOSE_AUTHORITY, TOKEN_PROGRAM_ID,
    };
    use magicblock_magic_program_api::instruction::MagicBlockInstruction;
    use solana_account::{AccountSharedData, ReadableAccount};
    use solana_instruction::{AccountMeta, Instruction};
    use solana_program::{program_option::COption, program_pack::Pack};
    use solana_sdk_ids::system_program;
    use spl_token::state::{
        Account as SplAccount, AccountState as SplAccountState,
    };

    use super::*;
    use crate::test_utils::process_instruction;

    fn rent_pending_ata_account(
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
            close_authority: COption::Some(RENT_PENDING_ATA_CLOSE_AUTHORITY),
        };
        let mut account =
            AccountSharedData::new(0, SplAccount::LEN, &TOKEN_PROGRAM_ID);
        SplAccount::pack(token_account, account.data_as_mut_slice()).unwrap();
        account.set_delegated(true);
        account
    }

    fn close_ix(owner: Pubkey, ata: Pubkey) -> Instruction {
        Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::CloseRentPendingAta,
            vec![
                AccountMeta::new_readonly(owner, true),
                AccountMeta::new(ata, false),
            ],
        )
    }

    #[test]
    fn close_rent_pending_ata_removes_drained_account() {
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
                (ata, rent_pending_ata_account(wallet_owner, mint, 0)),
            ],
            ix.accounts,
            Ok(()),
        );

        let ata_after = &accounts[1];
        assert_eq!(ata_after.lamports(), 0);
        assert_eq!(ata_after.owner(), &Pubkey::default());
        assert!(ata_after.data().is_empty());
        assert!(!ata_after.delegated());
        assert!(ata_after.ephemeral());
    }

    #[test]
    fn close_rent_pending_ata_noops_when_funded() {
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata = derive_ata(&wallet_owner, &mint);
        let funded = rent_pending_ata_account(wallet_owner, mint, 5);

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
        assert!(accounts[1].delegated());
    }

    #[test]
    fn close_rent_pending_ata_noops_for_other_signer() {
        let wallet_owner = Pubkey::new_unique();
        let other = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata = derive_ata(&wallet_owner, &mint);
        let drained = rent_pending_ata_account(wallet_owner, mint, 0);

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
        assert!(accounts[1].delegated());
    }

    #[test]
    fn close_rent_pending_ata_noops_for_missing_account() {
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
    fn close_rent_pending_ata_requires_owner_signature() {
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
                (ata, rent_pending_ata_account(wallet_owner, mint, 0)),
            ],
            ix.accounts,
            Err(InstructionError::MissingRequiredSignature),
        );
    }
}
