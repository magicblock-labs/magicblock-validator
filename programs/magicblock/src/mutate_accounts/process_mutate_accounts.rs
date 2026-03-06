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
    clone_account::{
        is_ephemeral, validate_not_delegated, validate_remote_slot,
    },
    errors::MagicBlockProgramError,
    validator::validator_authority_id,
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
    let validator_authority_acc = {
        // 1.1. MagicBlock authority must sign
        let validator_authority_id = validator_authority_id();
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
            .get_account_at_index(authority_transaction_index)?;
        if magicblock_authority_acc
            .borrow()
            .owner()
            .ne(&system_program::id())
        {
            ic_msg!(
                invoke_context,
                "MutateAccounts: MagicBlock authority needs to be owned by the system program"
            );
            return Err(
                MagicBlockProgramError::MagicBlockAuthorityNeedsToBeOwnedBySystemProgram
                    .into(),
            );
        }
        magicblock_authority_acc
    };

    let mut lamports_to_debit: i128 = 0;

    // 2. Apply account modifications
    for idx in 0..account_mods_len {
        // NOTE: first account is the MagicBlock authority, account mods start at second account
        let account_idx = (idx + 1) as u16;
        let account_transaction_index = instruction_context
            .get_index_of_instruction_account_in_transaction(account_idx)?;
        let account = transaction_context
            .get_account_at_index(account_transaction_index)?;

        // Skip ephemeral accounts (exist locally on ER only)
        if is_ephemeral(account) {
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

        let mut modification = account_mods.remove(account_key).ok_or_else(|| {
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
        validate_not_delegated(account, account_key, invoke_context)?;
        validate_remote_slot(
            account,
            account_key,
            modification.remote_slot,
            invoke_context,
        )?;

        // While an account is undelegating and the delegation is not completed,
        // we will never clone/mutate it. Thus we can safely untoggle this flag
        // here AFTER validation passes.
        account.borrow_mut().set_undelegating(false);

        if let Some(lamports) = modification.lamports {
            ic_msg!(
                invoke_context,
                "MutateAccounts: setting lamports to {}",
                lamports
            );
            let current_lamports = account.borrow().lamports();
            lamports_to_debit += lamports as i128 - current_lamports as i128;

            account.borrow_mut().set_lamports(lamports);
        }
        if let Some(owner) = modification.owner {
            ic_msg!(
                invoke_context,
                "MutateAccounts: setting owner to {}",
                owner
            );
            account.borrow_mut().set_owner(owner);
        }
        if let Some(executable) = modification.executable {
            ic_msg!(
                invoke_context,
                "MutateAccounts: setting executable to {}",
                executable
            );
            account.borrow_mut().set_executable(executable);
        }
        if let Some(data) = modification.data.take() {
            ic_msg!(
                invoke_context,
                "MutateAccounts: setting data to len {}",
                data.len()
            );
            account.borrow_mut().set_data_from_slice(&data);
        }
        if let Some(delegated) = modification.delegated {
            ic_msg!(
                invoke_context,
                "MutateAccounts: setting delegated to {}",
                delegated
            );
            account.borrow_mut().set_delegated(delegated);
        }
        if let Some(confined) = modification.confined {
            ic_msg!(
                invoke_context,
                "MutateAccounts: setting confined to {}",
                confined
            );
            account.borrow_mut().set_confined(confined);
        }
        if let Some(remote_slot) = modification.remote_slot {
            ic_msg!(
                invoke_context,
                "MutateAccounts: setting remote_slot to {}",
                remote_slot
            );
            account.borrow_mut().set_remote_slot(remote_slot);
        }
    }

    if lamports_to_debit != 0 {
        let authority_lamports = validator_authority_acc.borrow().lamports();
        let adjusted_authority_lamports = if lamports_to_debit > 0 {
            (authority_lamports as u128)
                .checked_sub(lamports_to_debit as u128)
                .ok_or(InstructionError::InsufficientFunds)
                .map_err(|err| {
                    ic_msg!(
                        invoke_context,
                        "MutateAccounts: not enough lamports in authority to debit: {}",
                        err
                    );
                    err
                })?
        } else {
            (authority_lamports as u128)
                .checked_add(lamports_to_debit.unsigned_abs())
                .ok_or(InstructionError::ArithmeticOverflow)
                .map_err(|err| {
                    ic_msg!(
                        invoke_context,
                        "MutateAccounts: too many lamports in authority to credit: {}",
                        err
                    );
                    err
                })?
        };

        validator_authority_acc.borrow_mut().set_lamports(
            u64::try_from(adjusted_authority_lamports).map_err(|err| {
                ic_msg!(
                    invoke_context,
                    "MutateAccounts: adjusted authority lamports overflow: {}",
                    err
                );
                InstructionError::ArithmeticOverflow
            })?,
        );
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
    fn test_mod_all_fields_of_one_account() {
        init_logger!();

        let owner_key = Pubkey::from([9; 32]);
        let mod_key = Pubkey::new_unique();
        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(mod_key, AccountSharedData::new(100, 0, &mod_key));
            map
        };
        ensure_started_validator(&mut account_data);

        let modification = AccountModification {
            pubkey: mod_key,
            lamports: Some(200),
            owner: Some(owner_key),
            executable: Some(true),
            data: Some(vec![1, 2, 3, 4, 5]),
            delegated: Some(true),
            confined: Some(true),
            remote_slot: None,
        };
        let ix = InstructionUtils::modify_accounts_instruction(
            vec![modification.clone()],
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
                assert_eq!(lamports, AUTHORITY_BALANCE - 100);
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
                lamports: 200,
                owner: owner_key,
                executable: true,
                data,
                rent_epoch: u64::MAX,
            } => {
                assert_eq!(data, modification.data.unwrap());
                assert_eq!(owner_key, modification.owner.unwrap());
            }
        );
    }

    #[test]
    fn test_mod_lamports_of_two_accounts() {
        init_logger!();

        let mod_key1 = Pubkey::new_unique();
        let mod_key2 = Pubkey::new_unique();
        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(mod_key1, AccountSharedData::new(100, 0, &mod_key1));
            map.insert(mod_key2, AccountSharedData::new(200, 0, &mod_key2));
            map
        };
        ensure_started_validator(&mut account_data);

        let ix = InstructionUtils::modify_accounts_instruction(
            vec![
                AccountModification {
                    pubkey: mod_key1,
                    lamports: Some(300),
                    ..AccountModification::default()
                },
                AccountModification {
                    pubkey: mod_key2,
                    lamports: Some(400),
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
                assert_eq!(lamports, AUTHORITY_BALANCE - 400);
                assert_eq!(owner, system_program::id());
                assert!(data.is_empty());
            }
        );
        let modified_account1 = accounts.drain(0..1).next().unwrap();
        assert!(!modified_account1.delegated());
        assert_matches!(
            modified_account1.into(),
            Account {
                lamports: 300,
                owner: _,
                executable: false,
                data,
                rent_epoch: u64::MAX,
            } => {
                assert!(data.is_empty());
            }
        );
        let modified_account2 = accounts.drain(0..1).next().unwrap();
        assert!(!modified_account2.delegated());
        assert_matches!(
            modified_account2.into(),
            Account {
                lamports: 400,
                owner: _,
                executable: false,
                data,
                rent_epoch: u64::MAX,
            } => {
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
        ensure_started_validator(&mut account_data);

        let ix = InstructionUtils::modify_accounts_instruction(
            vec![AccountModification {
                pubkey: mod_key,
                lamports: Some(200),
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
        ensure_started_validator(&mut account_data);

        let ix = InstructionUtils::modify_accounts_instruction(
            vec![AccountModification {
                pubkey: mod_key,
                lamports: Some(200),
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
        assert_matches!(
            modified_account.into(),
            Account {
                lamports: 200,
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

    #[test]
    fn test_mod_different_properties_of_four_accounts() {
        init_logger!();

        let mod_key1 = Pubkey::new_unique();
        let mod_key2 = Pubkey::new_unique();
        let mod_key3 = Pubkey::new_unique();
        let mod_key4 = Pubkey::new_unique();
        let mod_2_owner = Pubkey::from([9; 32]);

        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(mod_key1, AccountSharedData::new(100, 0, &mod_key1));
            map.insert(mod_key2, AccountSharedData::new(200, 0, &mod_key2));
            map.insert(mod_key3, AccountSharedData::new(300, 0, &mod_key3));
            map.insert(mod_key4, AccountSharedData::new(400, 0, &mod_key4));
            map
        };
        ensure_started_validator(&mut account_data);

        let ix = InstructionUtils::modify_accounts_instruction(
            vec![
                AccountModification {
                    pubkey: mod_key1,
                    lamports: Some(1000),
                    data: Some(vec![1, 2, 3, 4, 5]),
                    delegated: Some(true),
                    ..Default::default()
                },
                AccountModification {
                    pubkey: mod_key2,
                    owner: Some(mod_2_owner),
                    ..Default::default()
                },
                AccountModification {
                    pubkey: mod_key3,
                    lamports: Some(3000),
                    ..Default::default()
                },
                AccountModification {
                    pubkey: mod_key4,
                    lamports: Some(100),
                    executable: Some(true),
                    data: Some(vec![16, 17, 18, 19, 20]),
                    delegated: Some(true),
                    ..Default::default()
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
                assert_eq!(lamports, AUTHORITY_BALANCE - 3300);
                assert_eq!(owner, system_program::id());
                assert!(data.is_empty());
            }
        );

        let modified_account1 = accounts.drain(0..1).next().unwrap();
        assert!(modified_account1.delegated());
        assert_matches!(
            modified_account1.into(),
            Account {
                lamports: 1000,
                owner: _,
                executable: false,
                data,
                rent_epoch: u64::MAX,
            } => {
                assert_eq!(data, vec![1, 2, 3, 4, 5]);
            }
        );

        let modified_account2 = accounts.drain(0..1).next().unwrap();
        assert!(!modified_account2.delegated());
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

        let modified_account3 = accounts.drain(0..1).next().unwrap();
        assert!(!modified_account3.delegated());
        assert_matches!(
            modified_account3.into(),
            Account {
                lamports: 3000,
                owner: _,
                executable: false,
                data,
                rent_epoch: u64::MAX,
            } => {
                assert!(data.is_empty());
            }
        );

        let modified_account4 = accounts.drain(0..1).next().unwrap();
        assert!(modified_account4.delegated());
        assert_matches!(
            modified_account4.into(),
            Account {
                lamports: 100,
                owner: _,
                executable: true,
                data,
                rent_epoch: u64::MAX,
            } => {
                assert_eq!(data, vec![16, 17, 18, 19, 20]);
            }
        );
    }

    #[test]
    fn test_mod_remote_slot() {
        init_logger!();

        let mod_key = Pubkey::new_unique();
        let remote_slot = 12345u64;
        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(mod_key, AccountSharedData::new(100, 0, &mod_key));
            map
        };
        ensure_started_validator(&mut account_data);

        let ix = InstructionUtils::modify_accounts_instruction(
            vec![AccountModification {
                pubkey: mod_key,
                remote_slot: Some(remote_slot),
                ..Default::default()
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

        let _account_authority = accounts.drain(0..1).next().unwrap();
        let modified_account = accounts.drain(0..1).next().unwrap();
        assert_eq!(modified_account.remote_slot(), remote_slot);
    }

    #[test]
    fn test_mod_remote_slot_rejects_stale_update() {
        init_logger!();

        let mod_key = Pubkey::new_unique();
        let mut account = AccountSharedData::new(100, 0, &mod_key);
        account.set_remote_slot(100);
        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(mod_key, account);
            map
        };
        ensure_started_validator(&mut account_data);

        let ix = InstructionUtils::modify_accounts_instruction(
            vec![AccountModification {
                pubkey: mod_key,
                lamports: Some(200),
                remote_slot: Some(99),
                ..Default::default()
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
            Err(MagicBlockProgramError::OutOfOrderUpdate.into()),
        );
    }

    #[test]
    fn test_mod_remote_slot_allows_equal_update() {
        init_logger!();

        let mod_key = Pubkey::new_unique();
        let mut account = AccountSharedData::new(100, 0, &mod_key);
        account.set_remote_slot(100);
        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(mod_key, account);
            map
        };
        ensure_started_validator(&mut account_data);

        let ix = InstructionUtils::modify_accounts_instruction(
            vec![AccountModification {
                pubkey: mod_key,
                lamports: Some(200),
                remote_slot: Some(100),
                ..Default::default()
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

        accounts.remove(0); // authority
        let account = accounts.remove(0);
        assert_eq!(account.lamports(), 200);
        assert_eq!(account.remote_slot(), 100);
    }
}
