use std::collections::{HashMap, HashSet};

use magicblock_magic_program_api::instruction::AccountModificationForInstruction;
use solana_account::{ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;
use solana_transaction_context::TransactionContext;

use crate::{
    clone_account::validate_not_delegated, errors::MagicBlockProgramError,
    validator::effective_validator_authority_id,
};

pub(crate) fn process_mutate_accounts(
    signers: HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    account_mods: &mut HashMap<Pubkey, AccountModificationForInstruction>,
    message: Option<String>,
) -> Result<(), InstructionError> {
    let instruction_context =
        transaction_context.get_current_instruction_context()?;

    // First account is the MagicBlock authority
    let accounts_len = instruction_context.get_number_of_instruction_accounts();
    let accounts_to_mod_len = accounts_len - 1;
    let account_mods_len = account_mods.len() as u64;

    // 1. Checks
    {
        // 1.1. MagicBlock authority must sign
        let validator_authority_id = effective_validator_authority_id();
        if !signers.contains(&validator_authority_id) {
            ic_msg!(
                invoke_context,
                "Validator identity '{}' not in signers",
                validator_authority_id
            );
            return Err(InstructionError::MissingRequiredSignature);
        }

        // 1.2. Need to have some accounts to modify
        if accounts_to_mod_len == 0 {
            ic_msg!(invoke_context, "MutateAccounts: no accounts to modify");
            return Err(MagicBlockProgramError::NoAccountsToModify.into());
        }

        // 1.3. Number of accounts to modify must match number of account modifications
        if accounts_to_mod_len as u64 != account_mods_len {
            ic_msg!(
                    invoke_context,
                    "MutateAccounts: number of accounts to modify ({}) does not match number of account modifications ({})",
                    accounts_to_mod_len,
                    account_mods_len
                );
            return Err(
                MagicBlockProgramError::AccountsToModifyNotMatchingAccountModifications
                    .into(),
            );
        }

        // 1.4. Check that first account is the MagicBlock authority
        let authority_transaction_index = instruction_context
            .get_index_of_instruction_account_in_transaction(0)?;
        let magicblock_authority_key = transaction_context
            .get_key_of_account_at_index(authority_transaction_index)?;
        if magicblock_authority_key != &validator_authority_id {
            ic_msg!(
                invoke_context,
                "MutateAccounts: first account must be the MagicBlock authority"
            );
            return Err(
                MagicBlockProgramError::FirstAccountNeedsToBeMagicBlockAuthority.into(),
            );
        }
        let magicblock_authority_acc = transaction_context
            .accounts()
            .try_borrow_mut(authority_transaction_index)?;
        if magicblock_authority_acc.owner().ne(&system_program::id()) {
            ic_msg!(
                invoke_context,
                "MutateAccounts: MagicBlock authority needs to be owned by the system program"
            );
            return Err(
                MagicBlockProgramError::MagicBlockAuthorityNeedsToBeOwnedBySystemProgram
                    .into(),
            );
        }
    }

    // 2. Apply account modifications
    for idx in 0..account_mods_len {
        // NOTE: first account is the MagicBlock authority, account mods start at second account
        let account_idx = (idx + 1) as u16;
        let account_transaction_index = instruction_context
            .get_index_of_instruction_account_in_transaction(account_idx)?;
        let mut account = transaction_context
            .accounts()
            .try_borrow_mut(account_transaction_index)?;

        // Skip ephemeral accounts (exist locally on ER only)
        if account.ephemeral() {
            let key = transaction_context
                .get_key_of_account_at_index(account_transaction_index)?;
            account_mods.remove(key);
            ic_msg!(
                invoke_context,
                "MutateAccounts: skipping ephemeral account {}",
                key
            );
            continue;
        }

        let account_key = transaction_context
            .get_key_of_account_at_index(account_transaction_index)?;

        let modification = account_mods.remove(account_key).ok_or_else(|| {
            ic_msg!(
                invoke_context,
                "MutateAccounts: account modification for the provided key {} is missing",
                account_key
            );
            MagicBlockProgramError::AccountModificationMissing
        })?;

        ic_msg!(
            invoke_context,
            "MutateAccounts: modifying '{}'.",
            account_key
        );

        // If provided log the extra message to give more context to the user
        if let Some(ref msg) = message {
            ic_msg!(invoke_context, "MutateAccounts: {}", msg);
        }

        // Validate account is mutable
        validate_not_delegated(&account, account_key, invoke_context)?;

        // While an account is undelegating and the delegation is not completed,
        // we will never clone/mutate it. Thus we can safely untoggle this flag
        // here AFTER validation passes.
        account.set_undelegating(false);

        if let Some(owner) = modification.owner {
            ic_msg!(
                invoke_context,
                "MutateAccounts: setting owner to {}",
                owner
            );
            account.set_owner(owner);
        }
        if let Some(delegated) = modification.delegated {
            ic_msg!(
                invoke_context,
                "MutateAccounts: setting delegated to {}",
                delegated
            );
            account.set_delegated(delegated);
        }
        if let Some(confined) = modification.confined {
            ic_msg!(
                invoke_context,
                "MutateAccounts: setting confined to {}",
                confined
            );
            account.set_confined(confined);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use assert_matches::assert_matches;
    use magicblock_magic_program_api::instruction::AccountModification;
    use solana_account::{Account, AccountSharedData};
    use test_kit::init_logger;

    use super::*;
    use crate::{
        instruction_utils::InstructionUtils,
        test_utils::{
            ensure_started_validator, process_instruction, AUTHORITY_BALANCE,
        },
    };

    // -----------------
    // ModifyAccounts
    // -----------------
    #[test]
    fn test_mod_supported_fields_of_one_account() {
        init_logger!();

        let owner_key = Pubkey::from([9; 32]);
        let mod_key = Pubkey::new_unique();
        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(mod_key, AccountSharedData::new(100, 0, &mod_key));
            map
        };
        ensure_started_validator(&mut account_data, None);

        let ix = InstructionUtils::modify_accounts_instruction(
            vec![AccountModification {
                pubkey: mod_key,
                owner: Some(owner_key),
                delegated: Some(true),
                confined: Some(true),
            }],
            None,
        );
        let transaction_accounts = ix
            .accounts
            .iter()
            .flat_map(|acc| {
                account_data
                    .remove(&acc.pubkey)
                    .map(|shared_data| (acc.pubkey, shared_data))
            })
            .collect();

        let mut accounts = process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );

        assert_eq!(accounts.len(), 2);

        let account_authority: AccountSharedData =
            accounts.drain(0..1).next().unwrap();
        assert!(!account_authority.delegated());
        assert!(!account_authority.confined());
        assert_matches!(
            account_authority.into(),
            Account {
                lamports,
                owner,
                executable: false,
                data,
                rent_epoch: u64::MAX,
            } => {
                assert_eq!(lamports, AUTHORITY_BALANCE);
                assert_eq!(owner, system_program::id());
                assert!(data.is_empty());
            }
        );
        let modified_account: AccountSharedData =
            accounts.drain(0..1).next().unwrap();
        assert!(modified_account.delegated());
        assert!(modified_account.confined());
        assert_matches!(
            modified_account.into(),
            Account {
                lamports: 100,
                owner,
                executable: false,
                data,
                rent_epoch: u64::MAX,
            } => {
                assert_eq!(owner, owner_key);
                assert!(data.is_empty());
            }
        );
    }

    #[test]
    fn test_mod_supported_fields_of_two_accounts() {
        init_logger!();

        let mod_key1 = Pubkey::new_unique();
        let mod_key2 = Pubkey::new_unique();
        let mod_2_owner = Pubkey::from([9; 32]);
        let mut account2 = AccountSharedData::new(200, 0, &mod_key2);
        account2.set_confined(true);
        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(mod_key1, AccountSharedData::new(100, 0, &mod_key1));
            map.insert(mod_key2, account2);
            map
        };
        ensure_started_validator(&mut account_data, None);

        let ix = InstructionUtils::modify_accounts_instruction(
            vec![
                AccountModification {
                    pubkey: mod_key1,
                    delegated: Some(true),
                    confined: Some(true),
                    ..AccountModification::default()
                },
                AccountModification {
                    pubkey: mod_key2,
                    owner: Some(mod_2_owner),
                    confined: Some(false),
                    ..AccountModification::default()
                },
            ],
            None,
        );
        let transaction_accounts = ix
            .accounts
            .iter()
            .flat_map(|acc| {
                account_data
                    .remove(&acc.pubkey)
                    .map(|shared_data| (acc.pubkey, shared_data))
            })
            .collect();

        let mut accounts = process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );

        assert_eq!(accounts.len(), 3);

        let account_authority = accounts.drain(0..1).next().unwrap();
        assert!(!account_authority.delegated());
        assert_matches!(
            account_authority.into(),
            Account {
                lamports,
                owner,
                executable: false,
                data,
                rent_epoch: u64::MAX,
            } => {
                assert_eq!(lamports, AUTHORITY_BALANCE);
                assert_eq!(owner, system_program::id());
                assert!(data.is_empty());
            }
        );
        let modified_account1 = accounts.drain(0..1).next().unwrap();
        assert!(modified_account1.delegated());
        assert!(modified_account1.confined());
        assert_matches!(
            modified_account1.into(),
            Account {
                lamports: 100,
                owner,
                executable: false,
                data,
                rent_epoch: u64::MAX,
            } => {
                assert_eq!(owner, mod_key1);
                assert!(data.is_empty());
            }
        );
        let modified_account2 = accounts.drain(0..1).next().unwrap();
        assert!(!modified_account2.delegated());
        assert!(!modified_account2.confined());
        assert_matches!(
            modified_account2.into(),
            Account {
                lamports: 200,
                owner,
                executable: false,
                data,
                rent_epoch: u64::MAX,
            } => {
                assert_eq!(owner, mod_2_owner);
                assert!(data.is_empty());
            }
        );
    }

    #[test]
    fn test_mutate_fails_for_delegated_non_undelegating_account() {
        init_logger!();

        let mod_key = Pubkey::new_unique();
        let mut delegated_account = AccountSharedData::new(100, 0, &mod_key);
        delegated_account.set_delegated(true);

        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(mod_key, delegated_account);
            map
        };
        ensure_started_validator(&mut account_data, None);

        let ix = InstructionUtils::modify_accounts_instruction(
            vec![AccountModification {
                pubkey: mod_key,
                confined: Some(true),
                ..AccountModification::default()
            }],
            None,
        );
        let transaction_accounts = ix
            .accounts
            .iter()
            .flat_map(|acc| {
                account_data
                    .remove(&acc.pubkey)
                    .map(|shared_data| (acc.pubkey, shared_data))
            })
            .collect();

        let _accounts = process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(MagicBlockProgramError::AccountIsDelegated.into()),
        );
    }

    #[test]
    fn test_mutate_succeeds_for_delegated_undelegating_account() {
        init_logger!();

        let mod_key = Pubkey::new_unique();
        let mut undelegating_account = AccountSharedData::new(100, 0, &mod_key);
        undelegating_account.set_delegated(true);
        undelegating_account.set_undelegating(true);

        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(mod_key, undelegating_account);
            map
        };
        ensure_started_validator(&mut account_data, None);

        let ix = InstructionUtils::modify_accounts_instruction(
            vec![AccountModification {
                pubkey: mod_key,
                confined: Some(true),
                ..AccountModification::default()
            }],
            None,
        );
        let transaction_accounts = ix
            .accounts
            .iter()
            .flat_map(|acc| {
                account_data
                    .remove(&acc.pubkey)
                    .map(|shared_data| (acc.pubkey, shared_data))
            })
            .collect();

        let mut accounts = process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );

        // authority account
        let _account_authority: AccountSharedData =
            accounts.drain(0..1).next().unwrap();
        let modified_account: AccountSharedData =
            accounts.drain(0..1).next().unwrap();

        assert!(modified_account.delegated());
        assert!(!modified_account.undelegating());
        assert!(modified_account.confined());
        assert_matches!(
            modified_account.into(),
            Account {
                lamports: 100,
                owner,
                executable: false,
                data,
                rent_epoch: u64::MAX,
            } => {
                assert_eq!(owner, mod_key);
                assert!(data.is_empty());
            }
        );
    }
}
