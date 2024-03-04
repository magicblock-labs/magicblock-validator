use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicUsize, Ordering},
        RwLock,
    },
};

use lazy_static::lazy_static;
use solana_program_runtime::{ic_msg, invoke_context::InvokeContext};
use solana_sdk::{
    account::{ReadableAccount, WritableAccount},
    signer::Signer,
    transaction_context::TransactionContext,
};
use solana_sdk::{instruction::InstructionError, pubkey::Pubkey, system_program};

use crate::{
    sleipnir_authority,
    sleipnir_instruction::{AccountModificationForInstruction, SleipnirError, SleipnirInstruction},
};

use {
    solana_program_runtime::declare_process_instruction,
    solana_sdk::program_utils::limited_deserialize,
};
pub const DEFAULT_COMPUTE_UNITS: u64 = 150;

declare_process_instruction!(Entrypoint, DEFAULT_COMPUTE_UNITS, |invoke_context| {
    let transaction_context = &invoke_context.transaction_context;
    let instruction_context = transaction_context.get_current_instruction_context()?;
    let instruction_data = instruction_context.get_instruction_data();
    let instruction = limited_deserialize(instruction_data)?;
    let signers = instruction_context.get_signers(transaction_context)?;

    match instruction {
        SleipnirInstruction::ModifyAccounts(mut account_mods) => mutate_accounts(
            signers,
            invoke_context,
            transaction_context,
            &mut account_mods,
        ),
    }
});

lazy_static! {
    /// In order to modify large data chunks we cannot include all the data as part of the
    /// transaction.
    /// Instead we register data here _before_ invoking the actual instruction and when it is
    /// processed it resolved that data from the key that we provide in its place.
    static ref DATA_MODS: RwLock<HashMap<usize, Vec<u8>>> = RwLock::new(HashMap::new());
}

pub fn get_account_mod_data_id() -> usize {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn set_account_mod_data(data: Vec<u8>) -> usize {
    let id = get_account_mod_data_id();
    DATA_MODS
        .write()
        .expect("DATA_MODS poisoned")
        .insert(id, data);
    id
}

fn get_data(id: usize) -> Option<Vec<u8>> {
    DATA_MODS.write().expect("DATA_MODS poisoned").remove(&id)
}

fn mutate_accounts(
    signers: HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    account_mods: &mut HashMap<Pubkey, AccountModificationForInstruction>,
) -> Result<(), InstructionError> {
    let accounts_len = transaction_context.get_number_of_accounts();
    // First account is the Sleipnir authority
    // Last account is the implicit NativeLoader
    let accounts_to_mod_len = accounts_len - 2;
    let account_mods_len = account_mods.len() as u64;

    // 1. Checks
    let sleipnir_authority_acc = {
        // 1.1. Sleipnir authority must sign
        let sleipnir_authority = sleipnir_authority().pubkey();
        if !signers.contains(&sleipnir_authority) {
            ic_msg!(
                invoke_context,
                "{} not in signers",
                &sleipnir_authority.to_string()
            );
            return Err(InstructionError::MissingRequiredSignature);
        }

        // 1.2. Need to have some accounts to modify
        if accounts_to_mod_len == 0 {
            ic_msg!(invoke_context, "MutateAccounts: no accounts to modify");
            return Err(SleipnirError::NoAccountsToModify.into());
        }

        // 1.3. Number of accounts to modify must match number of account modifications
        if accounts_to_mod_len as u64 != account_mods_len {
            ic_msg!(
                    invoke_context,
                    "MutateAccounts: number of accounts to modify ({}) does not match number of account modifications ({})",
                    accounts_to_mod_len,
                    account_mods_len
                );
            return Err(SleipnirError::AccountsToModifyNotMatchingAccountModifications.into());
        }

        // 1.4. Check that first account is the Sleipnir authority
        let sleipnir_authority_key = transaction_context.get_key_of_account_at_index(0)?;
        if sleipnir_authority_key != &sleipnir_authority {
            ic_msg!(
                invoke_context,
                "MutateAccounts: first account must be the Sleipnir authority"
            );
            return Err(SleipnirError::FirstAccountNeedsToBeSleipnirAuthority.into());
        }
        let sleipnir_authority_acc = transaction_context.get_account_at_index(0)?;
        if sleipnir_authority_acc
            .borrow()
            .owner()
            .ne(&system_program::id())
        {
            ic_msg!(
                invoke_context,
                "MutateAccounts: Sleipnir authority needs to be owned by the system program"
            );
            return Err(SleipnirError::SleipnirAuthorityNeedsToBeOwnedBySystemProgram.into());
        }
        sleipnir_authority_acc
    };

    let mut lamports_to_debit: i128 = 0;

    // 2. Apply account modifications
    for idx in 0..accounts_to_mod_len {
        // NOTE: first account is the Sleipnir authority, account mods start at second account
        let account_idx = idx + 1;
        let account = transaction_context.get_account_at_index(account_idx)?;
        let account_key = transaction_context.get_key_of_account_at_index(account_idx)?;

        let mut modification = account_mods.remove(account_key).ok_or_else(|| {
            ic_msg!(
                invoke_context,
                "MutateAccounts: account modification for the provided key {} is missing",
                account_key
            );
            SleipnirError::AccountModificationMissing
        })?;

        if let Some(lamports) = modification.lamports {
            let current_lamports = account.borrow().lamports();
            lamports_to_debit += lamports as i128 - current_lamports as i128;

            account.borrow_mut().set_lamports(lamports);
        }
        if let Some(owner) = modification.owner {
            account.borrow_mut().set_owner(owner);
        }
        if let Some(executable) = modification.executable {
            account.borrow_mut().set_executable(executable);
        }
        if let Some(data_key) = modification.data_key.take() {
            let data = get_data(data_key)
                .ok_or(SleipnirError::AccountDataMissing)
                .map_err(|err| {
                    ic_msg!(
                        invoke_context,
                        "MutateAccounts: account data for the provided key {} is missing",
                        data_key
                    );
                    err
                })?;
            account.borrow_mut().set_data_from_slice(data.as_slice());
        }
        if let Some(rent_epoch) = modification.rent_epoch {
            account.borrow_mut().set_rent_epoch(rent_epoch);
        }
    }

    if lamports_to_debit != 0 {
        let authority_lamports = sleipnir_authority_acc.borrow().lamports();
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
                        "MutateAccounts: too much lamports in authority to credit: {}",
                        err
                    );
                    err
                })?
        };

        sleipnir_authority_acc.borrow_mut().set_lamports(
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

/* TODO: Figure out how to mock_process_instruction and enable these tests
#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use std::collections::HashMap;

    use crate::{
        sleipnir_authority,
        sleipnir_instruction::{modify_accounts_instruction, AccountModification},
    };

    use super::*;
    use solana_program::{
        instruction::{AccountMeta, InstructionError},
        pubkey::Pubkey,
        sleipnir_program,
    };
    use solana_program_runtime::invoke_context::mock_process_instruction;
    use solana_sdk::{
        account::{Account, AccountSharedData},
        signer::Signer,
    };

    fn process_instruction(
        instruction_data: &[u8],
        transaction_accounts: Vec<(Pubkey, AccountSharedData)>,
        instruction_accounts: Vec<AccountMeta>,
        expected_result: Result<(), InstructionError>,
    ) -> Vec<AccountSharedData> {
        mock_process_instruction(
            &sleipnir_program::id(),
            Vec::new(),
            instruction_data,
            transaction_accounts,
            instruction_accounts,
            expected_result,
            Entrypoint::vm,
            |_invoke_context| {},
            |_invoke_context| {},
        )
    }

    #[test]
    fn test_mod_all_fields_of_one_account() {
        let owner_key = Pubkey::from([9; 32]);
        let mod_key = Pubkey::new_unique();
        let authority_balance = u64::MAX / 2;
        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(mod_key, AccountSharedData::new(100, 0, &mod_key));
            // NOTE: this needs to be added at genesis
            map.insert(
                sleipnir_authority().pubkey(),
                AccountSharedData::new(authority_balance, 0, &system_program::id()),
            );
            map
        };
        let modification = AccountModification {
            lamports: Some(200),
            owner: Some(owner_key),
            executable: Some(true),
            data: Some(vec![1, 2, 3, 4, 5]),
            rent_epoch: Some(88),
        };
        let ix = modify_accounts_instruction(vec![(mod_key, modification.clone())]);
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

        let account_authority: Account = accounts.drain(0..1).next().unwrap().into();
        assert_matches!(
            account_authority,
            Account {
                lamports,
                owner,
                executable: false,
                data,
                rent_epoch: 0,
            } => {
                assert_eq!(lamports, authority_balance - 100);
                assert_eq!(owner, system_program::id());
                assert!(data.is_empty());
            }
        );
        let modified_account: Account = accounts.drain(0..1).next().unwrap().into();
        assert_matches!(
            modified_account,
            Account {
                lamports: 200,
                owner: owner_key,
                executable: true,
                data,
                rent_epoch: 88,
            } => {
                assert_eq!(data, modification.data.unwrap());
                assert_eq!(owner_key, modification.owner.unwrap());
            }
        );
    }

    #[test]
    fn test_mod_lamports_of_two_accounts() {
        let mod_key1 = Pubkey::new_unique();
        let mod_key2 = Pubkey::new_unique();
        let authority_balance = u64::MAX / 2;
        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(mod_key1, AccountSharedData::new(100, 0, &mod_key1));
            map.insert(mod_key2, AccountSharedData::new(200, 0, &mod_key2));
            // NOTE: this needs to be added at genesis
            map.insert(
                sleipnir_authority().pubkey(),
                AccountSharedData::new(authority_balance, 0, &system_program::id()),
            );
            map
        };
        let ix = modify_accounts_instruction(vec![
            (
                mod_key1,
                AccountModification {
                    lamports: Some(300),
                    ..AccountModification::default()
                },
            ),
            (
                mod_key2,
                AccountModification {
                    lamports: Some(400),
                    ..AccountModification::default()
                },
            ),
        ]);
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

        let account_authority: Account = accounts.drain(0..1).next().unwrap().into();
        assert_matches!(
            account_authority,
            Account {
                lamports,
                owner,
                executable: false,
                data,
                rent_epoch: 0,
            } => {
                assert_eq!(lamports, authority_balance - 400);
                assert_eq!(owner, system_program::id());
                assert!(data.is_empty());
            }
        );
        let modified_account1: Account = accounts.drain(0..1).next().unwrap().into();
        assert_matches!(
            modified_account1,
            Account {
                lamports: 300,
                owner: _,
                executable: false,
                data,
                rent_epoch: 0,
            } => {
                assert!(data.is_empty());
            }
        );
        let modified_account2: Account = accounts.drain(0..1).next().unwrap().into();
        assert_matches!(
            modified_account2,
            Account {
                lamports: 400,
                owner: _,
                executable: false,
                data,
                rent_epoch: 0,
            } => {
                assert!(data.is_empty());
            }
        );
    }

    #[test]
    fn test_mod_different_properties_of_four_accounts() {
        let mod_key1 = Pubkey::new_unique();
        let mod_key2 = Pubkey::new_unique();
        let mod_key3 = Pubkey::new_unique();
        let mod_key4 = Pubkey::new_unique();
        let mod_2_owner = Pubkey::from([9; 32]);

        let authority_balance = u64::MAX / 2;
        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(mod_key1, AccountSharedData::new(100, 0, &mod_key1));
            map.insert(mod_key2, AccountSharedData::new(200, 0, &mod_key2));
            map.insert(mod_key3, AccountSharedData::new(300, 0, &mod_key3));
            map.insert(mod_key4, AccountSharedData::new(400, 0, &mod_key4));
            // NOTE: this needs to be added at genesis
            map.insert(
                sleipnir_authority().pubkey(),
                AccountSharedData::new(authority_balance, 0, &system_program::id()),
            );
            map
        };
        let ix = modify_accounts_instruction(vec![
            (
                mod_key1,
                AccountModification {
                    lamports: Some(1000),
                    data: Some(vec![1, 2, 3, 4, 5]),
                    ..Default::default()
                },
            ),
            (
                mod_key2,
                AccountModification {
                    owner: Some(mod_2_owner),
                    ..Default::default()
                },
            ),
            (
                mod_key3,
                AccountModification {
                    lamports: Some(3000),
                    rent_epoch: Some(90),
                    ..Default::default()
                },
            ),
            (
                mod_key4,
                AccountModification {
                    lamports: Some(100),
                    executable: Some(true),
                    data: Some(vec![16, 17, 18, 19, 20]),
                    rent_epoch: Some(91),
                    ..Default::default()
                },
            ),
        ]);

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

        let account_authority: Account = accounts.drain(0..1).next().unwrap().into();
        assert_matches!(
            account_authority,
            Account {
                lamports,
                owner,
                executable: false,
                data,
                rent_epoch: 0,
            } => {
                assert_eq!(lamports, authority_balance - 3300);
                assert_eq!(owner, system_program::id());
                assert!(data.is_empty());
            }
        );

        let modified_account1: Account = accounts.drain(0..1).next().unwrap().into();
        assert_matches!(
            modified_account1,
            Account {
                lamports: 1000,
                owner: _,
                executable: false,
                data,
                rent_epoch: 0,
            } => {
                assert_eq!(data, vec![1, 2, 3, 4, 5]);
            }
        );

        let modified_account2: Account = accounts.drain(0..1).next().unwrap().into();
        assert_matches!(
            modified_account2,
            Account {
                lamports: 200,
                owner,
                executable: false,
                data,
                rent_epoch: 0,
            } => {
                assert_eq!(owner, mod_2_owner);
                assert!(data.is_empty());
            }
        );

        let modified_account3: Account = accounts.drain(0..1).next().unwrap().into();
        assert_matches!(
            modified_account3,
            Account {
                lamports: 3000,
                owner: _,
                executable: false,
                data,
                rent_epoch: 90,
            } => {
                assert!(data.is_empty());
            }
        );

        let modified_account4: Account = accounts.drain(0..1).next().unwrap().into();
        assert_matches!(
            modified_account4,
            Account {
                lamports: 100,
                owner: _,
                executable: true,
                data,
                rent_epoch: 91,
            } => {
                assert_eq!(data, vec![16, 17, 18, 19, 20]);
            }
        );
    }
}
*/
