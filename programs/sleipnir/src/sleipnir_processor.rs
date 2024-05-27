use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicUsize, Ordering},
        RwLock,
    },
};

use lazy_static::lazy_static;
use solana_program_runtime::{
    declare_process_instruction, ic_msg, invoke_context::InvokeContext,
};
use solana_sdk::{
    account::{ReadableAccount, WritableAccount},
    instruction::InstructionError,
    program_utils::limited_deserialize,
    pubkey::Pubkey,
    system_program,
    transaction_context::TransactionContext,
};

use crate::{
    commit_sender::send_commit,
    errors::MagicError,
    sleipnir_instruction::{
        AccountModificationForInstruction, SleipnirError, SleipnirInstruction,
    },
    validator_authority_id,
};
pub const DEFAULT_COMPUTE_UNITS: u64 = 150;

declare_process_instruction!(
    Entrypoint,
    DEFAULT_COMPUTE_UNITS,
    |invoke_context| {
        let transaction_context = &invoke_context.transaction_context;
        let instruction_context =
            transaction_context.get_current_instruction_context()?;
        let instruction_data = instruction_context.get_instruction_data();
        let instruction = limited_deserialize(instruction_data)?;
        let signers = instruction_context.get_signers(transaction_context)?;

        match instruction {
            SleipnirInstruction::ModifyAccounts(mut account_mods) => {
                mutate_accounts(
                    signers,
                    invoke_context,
                    transaction_context,
                    &mut account_mods,
                )
            }
            SleipnirInstruction::TriggerCommit => {
                trigger_commit(signers, invoke_context, transaction_context)
            }
        }
    }
);

// -----------------
// MutateAccounts
// -----------------
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
    let validator_authority_acc = {
        // 1.1. Sleipnir authority must sign
        let validator_authority = validator_authority_id();
        if !signers.contains(&validator_authority) {
            ic_msg!(
                invoke_context,
                "Validator identity '{}' not in signers",
                &validator_authority.to_string()
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
            return Err(
                SleipnirError::AccountsToModifyNotMatchingAccountModifications
                    .into(),
            );
        }

        // 1.4. Check that first account is the Sleipnir authority
        let sleipnir_authority_key =
            transaction_context.get_key_of_account_at_index(0)?;
        if sleipnir_authority_key != &validator_authority {
            ic_msg!(
                invoke_context,
                "MutateAccounts: first account must be the Sleipnir authority"
            );
            return Err(
                SleipnirError::FirstAccountNeedsToBeSleipnirAuthority.into()
            );
        }
        let sleipnir_authority_acc =
            transaction_context.get_account_at_index(0)?;
        if sleipnir_authority_acc
            .borrow()
            .owner()
            .ne(&system_program::id())
        {
            ic_msg!(
                invoke_context,
                "MutateAccounts: Sleipnir authority needs to be owned by the system program"
            );
            return Err(
                SleipnirError::SleipnirAuthorityNeedsToBeOwnedBySystemProgram
                    .into(),
            );
        }
        sleipnir_authority_acc
    };

    let mut lamports_to_debit: i128 = 0;

    // 2. Apply account modifications
    for idx in 0..accounts_to_mod_len {
        // NOTE: first account is the Sleipnir authority, account mods start at second account
        let account_idx = idx + 1;
        let account = transaction_context.get_account_at_index(account_idx)?;
        let account_key =
            transaction_context.get_key_of_account_at_index(account_idx)?;

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
                        "MutateAccounts: too much lamports in authority to credit: {}",
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

// -----------------
// TriggerCommit
// -----------------
fn trigger_commit(
    signers: HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
) -> Result<(), InstructionError> {
    let validator_authority = validator_authority_id();
    if !signers.contains(&validator_authority) {
        ic_msg!(
            invoke_context,
            "Validator identity '{}' not in signers",
            &validator_authority.to_string()
        );
        return Err(InstructionError::MissingRequiredSignature);
    }
    // Validator authority is the first account and the account to commit is the second
    let pubkey = transaction_context.get_key_of_account_at_index(1)?;

    ic_msg!(invoke_context, "TriggerCommit: for account {}", pubkey);
    send_commit(*pubkey)
        // Handle error related to sending the request
        .map_err(|err| {
            ic_msg!(
                invoke_context,
                "TriggerCommit: failed to send commit pubkey: {}.\nError: {}",
                pubkey,
                err
            );
            InstructionError::from(err.error)
        })?
        .blocking_recv()
        // Handle error related to receiving request confirmation
        .map_err(|err| {
            ic_msg!(
                invoke_context,
                "TriggerCommit: failed to receive commit pubkey response: {} ({:?})",
                pubkey,
                err
            );
            InstructionError::from(MagicError::InternalError)
        })?
        // Handle error related to processing the request
        .map_err(|err| {
            ic_msg!(
                invoke_context,
                "TriggerCommit: failed to process commit pubkey: {}.\nError: {}",
                pubkey,
                err
            );
            InstructionError::from(err.error)
        })?;

    Ok(())
}

// mock_process_instruction
#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use std::collections::HashMap;
    use tokio::sync::mpsc;

    use crate::{
        commit_sender::{set_commit_sender, TriggerCommitCallback},
        errors::MagicErrorWithContext,
        has_validator_authority, set_validator_authority,
        sleipnir_instruction::{
            modify_accounts_instruction, trigger_commit_instruction,
            AccountModification,
        },
    };

    use super::*;
    use solana_program_runtime::invoke_context::mock_process_instruction;
    use solana_sdk::{
        account::{Account, AccountSharedData},
        signature::{Keypair, Signature},
        signer::Signer,
    };
    use solana_sdk::{
        instruction::{AccountMeta, InstructionError},
        pubkey::Pubkey,
    };

    const AUTHORITY_BALANCE: u64 = u64::MAX / 2;

    pub fn ensure_funded_validator_authority(
        map: &mut HashMap<Pubkey, AccountSharedData>,
    ) {
        if !has_validator_authority() {
            let validator_authority = Keypair::new();
            let validator_id = validator_authority.pubkey();
            set_validator_authority(validator_authority);

            map.insert(
                validator_id,
                AccountSharedData::new(
                    AUTHORITY_BALANCE,
                    0,
                    &system_program::id(),
                ),
            );
        }
    }

    fn process_instruction(
        instruction_data: &[u8],
        transaction_accounts: Vec<(Pubkey, AccountSharedData)>,
        instruction_accounts: Vec<AccountMeta>,
        expected_result: Result<(), InstructionError>,
    ) -> Vec<AccountSharedData> {
        mock_process_instruction(
            &crate::id(),
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

    // -----------------
    // ModifyAccounts
    // -----------------
    #[test]
    fn test_mod_all_fields_of_one_account() {
        let owner_key = Pubkey::from([9; 32]);
        let mod_key = Pubkey::new_unique();
        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(mod_key, AccountSharedData::new(100, 0, &mod_key));
            map
        };
        ensure_funded_validator_authority(&mut account_data);

        let modification = AccountModification {
            lamports: Some(200),
            owner: Some(owner_key),
            executable: Some(true),
            data: Some(vec![1, 2, 3, 4, 5]),
            rent_epoch: Some(88),
        };
        let ix =
            modify_accounts_instruction(vec![(mod_key, modification.clone())]);
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

        let account_authority: Account =
            accounts.drain(0..1).next().unwrap().into();
        assert_matches!(
            account_authority,
            Account {
                lamports,
                owner,
                executable: false,
                data,
                rent_epoch: 0,
            } => {
                assert_eq!(lamports, AUTHORITY_BALANCE - 100);
                assert_eq!(owner, system_program::id());
                assert!(data.is_empty());
            }
        );
        let modified_account: Account =
            accounts.drain(0..1).next().unwrap().into();
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
        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(mod_key1, AccountSharedData::new(100, 0, &mod_key1));
            map.insert(mod_key2, AccountSharedData::new(200, 0, &mod_key2));
            map
        };
        ensure_funded_validator_authority(&mut account_data);

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

        let account_authority: Account =
            accounts.drain(0..1).next().unwrap().into();
        assert_matches!(
            account_authority,
            Account {
                lamports,
                owner,
                executable: false,
                data,
                rent_epoch: 0,
            } => {
                assert_eq!(lamports, AUTHORITY_BALANCE - 400);
                assert_eq!(owner, system_program::id());
                assert!(data.is_empty());
            }
        );
        let modified_account1: Account =
            accounts.drain(0..1).next().unwrap().into();
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
        let modified_account2: Account =
            accounts.drain(0..1).next().unwrap().into();
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

        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(mod_key1, AccountSharedData::new(100, 0, &mod_key1));
            map.insert(mod_key2, AccountSharedData::new(200, 0, &mod_key2));
            map.insert(mod_key3, AccountSharedData::new(300, 0, &mod_key3));
            map.insert(mod_key4, AccountSharedData::new(400, 0, &mod_key4));
            map
        };
        ensure_funded_validator_authority(&mut account_data);

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

        let account_authority: Account =
            accounts.drain(0..1).next().unwrap().into();
        assert_matches!(
            account_authority,
            Account {
                lamports,
                owner,
                executable: false,
                data,
                rent_epoch: 0,
            } => {
                assert_eq!(lamports, AUTHORITY_BALANCE - 3300);
                assert_eq!(owner, system_program::id());
                assert!(data.is_empty());
            }
        );

        let modified_account1: Account =
            accounts.drain(0..1).next().unwrap().into();
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

        let modified_account2: Account =
            accounts.drain(0..1).next().unwrap().into();
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

        let modified_account3: Account =
            accounts.drain(0..1).next().unwrap().into();
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

        let modified_account4: Account =
            accounts.drain(0..1).next().unwrap().into();
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

    // -----------------
    // TriggerCommit
    // -----------------
    fn setup_commit_handler(delegated_key: Pubkey) {
        let (tx, mut rx) = mpsc::channel::<(Pubkey, TriggerCommitCallback)>(5);
        set_commit_sender(tx);

        tokio::task::spawn(async move {
            let (pubkey, cb) = rx.recv().await.unwrap();
            if pubkey == delegated_key {
                cb.send(Ok(Signature::default()))
            } else {
                cb.send(Err(MagicErrorWithContext::new(
                    MagicError::AccountNotDelegated,
                    format!("Account: '{}'", pubkey),
                )))
            }
        });
    }

    #[tokio::test]
    async fn test_trigger_commit_for_delegated_account() {
        let delegated_key = Pubkey::new_unique();
        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(
                delegated_key,
                AccountSharedData::new(100, 0, &delegated_key),
            );
            map
        };
        ensure_funded_validator_authority(&mut account_data);
        setup_commit_handler(delegated_key);

        let ix = trigger_commit_instruction(delegated_key);

        let transaction_accounts = ix
            .accounts
            .iter()
            .flat_map(|acc| {
                account_data
                    .remove(&acc.pubkey)
                    .map(|shared_data| (acc.pubkey, shared_data))
            })
            .collect();

        tokio::task::spawn_blocking(move || {
            process_instruction(
                ix.data.as_slice(),
                transaction_accounts,
                ix.accounts,
                Ok(()),
            );
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_trigger_commit_for_undelegated_account() {
        let delegated_key = Pubkey::new_unique();
        let undelegated_key = Pubkey::new_unique();
        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(
                delegated_key,
                AccountSharedData::new(100, 0, &delegated_key),
            );
            map.insert(
                undelegated_key,
                AccountSharedData::new(100, 0, &undelegated_key),
            );
            map
        };
        ensure_funded_validator_authority(&mut account_data);
        setup_commit_handler(delegated_key);

        let ix = trigger_commit_instruction(undelegated_key);

        let transaction_accounts = ix
            .accounts
            .iter()
            .flat_map(|acc| {
                account_data
                    .remove(&acc.pubkey)
                    .map(|shared_data| (acc.pubkey, shared_data))
            })
            .collect();

        tokio::task::spawn_blocking(move || {
            process_instruction(
                ix.data.as_slice(),
                transaction_accounts,
                ix.accounts,
                Err(InstructionError::from(MagicError::AccountNotDelegated)),
            );
        })
        .await
        .unwrap();
    }
}
