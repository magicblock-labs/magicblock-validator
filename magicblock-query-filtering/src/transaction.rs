use magicblock_accounts_db::traits::AccountsBank;
use solana_account_decoder_client_types::token::UiTokenAmount;
use solana_message::{
    compiled_instruction::CompiledInstruction, v0::Message as MessageV0,
    Message as LegacyMessage, MessageHeader, VersionedMessage,
};
use solana_pubkey::{pubkey, Pubkey};
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_status::{
    ConfirmedTransactionWithStatusMeta, TransactionStatusMeta,
    TransactionTokenBalance, TransactionWithStatusMeta,
    VersionedTransactionWithStatusMeta,
};

use crate::{
    permissions_for_accounts,
    service::{QueryFilteringError, QueryFilteringResult},
    types::Access,
};

const SYSTEM_PROGRAM_ID: Pubkey = pubkey!("11111111111111111111111111111111");

/// Programs whose instructions are always allowed, even when the accounts
/// they touch are restricted.
pub const WHITELISTED_PROGRAMS: &[Pubkey] = &[
    SYSTEM_PROGRAM_ID,
    pubkey!("ComputeBudget111111111111111111111111111111"),
    pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"),
    pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"),
    pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"),
    pubkey!("Ed25519SigVerify111111111111111111111111111"),
    pubkey!("ACLseoPoyC3cBqoUtkbjZ4aDrkurZW86v19pXz2XQnp1"),
    pubkey!("SPLxh1LVZzEkX99H6rqYizhytLWPZVV296zyYDPagv2"),
    pubkey!("Magic11111111111111111111111111111111111111"),
];

/// Rejects a transaction submission when any non-whitelisted instruction
/// reads an account whose permission entry restricts the invoked program.
///
/// The check intentionally ignores the calling user: program-level access is
/// controlled by listing the program pubkey as a member of the account's
/// permission entry.
pub fn check_transaction_admission(
    accounts_db: &impl AccountsBank,
    static_account_keys: &[Pubkey],
    instructions: &[CompiledInstruction],
) -> QueryFilteringResult<()> {
    let permissions =
        permissions_for_accounts(accounts_db, static_account_keys)?;

    for ix in instructions {
        let Some(program_id) =
            static_account_keys.get(ix.program_id_index as usize)
        else {
            continue;
        };
        if WHITELISTED_PROGRAMS.contains(program_id) {
            continue;
        }
        for &account_index in &ix.accounts {
            let Some(permission) = permissions.get(account_index as usize)
            else {
                continue;
            };
            if permission.is_program_restricted(program_id) {
                return Err(QueryFilteringError::AccessDenied);
            }
        }
    }

    Ok(())
}

/// Convenience overload that extracts static keys and instructions from a
/// [`VersionedMessage`] before delegating to [`check_transaction_admission`].
pub fn check_versioned_message_admission(
    accounts_db: &impl AccountsBank,
    message: &VersionedMessage,
) -> QueryFilteringResult<()> {
    check_transaction_admission(
        accounts_db,
        message.static_account_keys(),
        message.instructions(),
    )
}

/// Mutates a confirmed transaction in place, redacting fields the caller
/// cannot see according to per-account permission flags.
///
/// When any account in the transaction is invisible to the user, the
/// transaction body is reduced to its signatures and the meta is reduced to
/// its status. Otherwise, balances/logs/message are individually redacted
/// based on per-member access flags.
pub fn filter_confirmed_transaction(
    accounts_db: &impl AccountsBank,
    confirmed: &mut ConfirmedTransactionWithStatusMeta,
    user: &Pubkey,
) -> QueryFilteringResult<()> {
    match &mut confirmed.tx_with_meta {
        TransactionWithStatusMeta::Complete(complete) => {
            filter_complete_transaction(accounts_db, complete, user)
        }
        TransactionWithStatusMeta::MissingMetadata(legacy) => {
            let permissions = permissions_for_accounts(
                accounts_db,
                &legacy.message.account_keys,
            )?;
            let any_invisible = permissions
                .iter()
                .any(|permission| !permission.access_for(user).account);
            if any_invisible {
                let blockhash = legacy.message.recent_blockhash;
                legacy.message = LegacyMessage {
                    header: MessageHeader::default(),
                    account_keys: vec![],
                    recent_blockhash: blockhash,
                    instructions: vec![],
                };
            }
            Ok(())
        }
    }
}

fn filter_complete_transaction(
    accounts_db: &impl AccountsBank,
    complete: &mut VersionedTransactionWithStatusMeta,
    user: &Pubkey,
) -> QueryFilteringResult<()> {
    let mut all_keys: Vec<Pubkey> =
        complete.transaction.message.static_account_keys().to_vec();
    all_keys.extend(complete.meta.loaded_addresses.writable.iter().copied());
    all_keys.extend(complete.meta.loaded_addresses.readonly.iter().copied());

    let permissions = permissions_for_accounts(accounts_db, &all_keys)?;
    let accesses: Vec<Access> = permissions
        .iter()
        .map(|permission| permission.access_for(user))
        .collect();

    if accesses.iter().any(|access| !access.account) {
        redact_full(complete);
        return Ok(());
    }

    if !accesses.iter().all(|access| access.balances) {
        zero_balances(&mut complete.meta, &accesses);
    }
    if !accesses.iter().all(|access| access.logs) {
        complete.meta.log_messages = Some(vec![]);
    }
    if !accesses.iter().all(|access| access.message) {
        redact_message(&mut complete.transaction);
    }

    Ok(())
}

fn redact_full(complete: &mut VersionedTransactionWithStatusMeta) {
    redact_message(&mut complete.transaction);
    complete.meta = TransactionStatusMeta {
        status: complete.meta.status.clone(),
        ..TransactionStatusMeta::default()
    };
}

fn redact_message(transaction: &mut VersionedTransaction) {
    let blockhash = *transaction.message.recent_blockhash();
    transaction.message = VersionedMessage::V0(MessageV0 {
        header: MessageHeader::default(),
        account_keys: vec![],
        recent_blockhash: blockhash,
        instructions: vec![],
        address_table_lookups: vec![],
    });
}

fn zero_balances(meta: &mut TransactionStatusMeta, accesses: &[Access]) {
    for (balance, access) in meta.pre_balances.iter_mut().zip(accesses) {
        if !access.balances {
            *balance = 0;
        }
    }
    for (balance, access) in meta.post_balances.iter_mut().zip(accesses) {
        if !access.balances {
            *balance = 0;
        }
    }
    if let Some(balances) = meta.pre_token_balances.as_mut() {
        zero_token_balances(balances, accesses);
    }
    if let Some(balances) = meta.post_token_balances.as_mut() {
        zero_token_balances(balances, accesses);
    }
}

fn zero_token_balances(
    balances: &mut [TransactionTokenBalance],
    accesses: &[Access],
) {
    for balance in balances.iter_mut() {
        let Some(access) = accesses.get(balance.account_index as usize) else {
            continue;
        };
        if !access.balances {
            balance.mint = SYSTEM_PROGRAM_ID.to_string();
            balance.owner = String::new();
            balance.program_id = String::new();
            balance.ui_token_amount = UiTokenAmount {
                ui_amount: None,
                decimals: 0,
                amount: "0".to_string(),
                ui_amount_string: "0.0".to_string(),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use magicblock_accounts_db::AccountsDbResult;
    use solana_account::AccountSharedData;
    use solana_hash::Hash;
    use solana_message::{
        compiled_instruction::CompiledInstruction, MessageHeader,
    };
    use solana_signature::Signature;

    use super::*;
    use crate::{
        types::{Member, Permission},
        PERMISSION_PROGRAM_ID,
    };

    const FLAG_LOGS: u8 = 1 << 1;
    const FLAG_BALANCES: u8 = 1 << 2;
    const FLAG_MESSAGE: u8 = 1 << 3;

    #[derive(Default)]
    struct MockAccountsBank {
        accounts: HashMap<Pubkey, AccountSharedData>,
    }

    impl MockAccountsBank {
        fn insert_permission(&mut self, account: Pubkey, members: Vec<Member>) {
            let permission = Permission::new(account, Some(members));
            let encoded = permission.encode(false);
            let mut data = AccountSharedData::new(
                1_000_000,
                encoded.len(),
                &PERMISSION_PROGRAM_ID,
            );
            data.set_data_from_slice(&encoded);
            self.accounts.insert(Permission::pda(&account), data);
        }
    }

    impl AccountsBank for MockAccountsBank {
        fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
            self.accounts.get(pubkey).cloned()
        }

        fn remove_account(&self, _pubkey: &Pubkey) {}

        fn remove_where(
            &self,
            _predicate: impl FnMut(&Pubkey, &AccountSharedData) -> bool,
        ) -> AccountsDbResult<usize> {
            Ok(0)
        }
    }

    fn versioned_message(
        keys: Vec<Pubkey>,
        instructions: Vec<CompiledInstruction>,
    ) -> VersionedMessage {
        VersionedMessage::V0(MessageV0 {
            header: MessageHeader {
                num_required_signatures: 1,
                num_readonly_signed_accounts: 0,
                num_readonly_unsigned_accounts: 0,
            },
            account_keys: keys,
            recent_blockhash: Hash::default(),
            instructions,
            address_table_lookups: vec![],
        })
    }

    fn complete_tx(
        keys: Vec<Pubkey>,
        meta: TransactionStatusMeta,
    ) -> VersionedTransactionWithStatusMeta {
        VersionedTransactionWithStatusMeta {
            transaction: VersionedTransaction {
                signatures: vec![Signature::default()],
                message: versioned_message(keys, vec![]),
            },
            meta,
        }
    }

    #[test]
    fn admission_allows_whitelisted_program_on_restricted_account() {
        let mut accounts = MockAccountsBank::default();
        let restricted = Pubkey::new_unique();
        accounts.insert_permission(restricted, vec![]);

        let message = versioned_message(
            vec![SYSTEM_PROGRAM_ID, restricted],
            vec![CompiledInstruction {
                program_id_index: 0,
                accounts: vec![1],
                data: vec![],
            }],
        );

        assert!(check_versioned_message_admission(&accounts, &message).is_ok());
    }

    #[test]
    fn admission_rejects_non_whitelisted_program_on_restricted_account() {
        let mut accounts = MockAccountsBank::default();
        let restricted = Pubkey::new_unique();
        let custom_program = Pubkey::new_unique();
        accounts.insert_permission(restricted, vec![]);

        let message = versioned_message(
            vec![custom_program, restricted],
            vec![CompiledInstruction {
                program_id_index: 0,
                accounts: vec![1],
                data: vec![],
            }],
        );

        assert!(matches!(
            check_versioned_message_admission(&accounts, &message),
            Err(QueryFilteringError::AccessDenied)
        ));
    }

    #[test]
    fn admission_allows_program_listed_as_member() {
        let mut accounts = MockAccountsBank::default();
        let restricted = Pubkey::new_unique();
        let custom_program = Pubkey::new_unique();
        accounts.insert_permission(
            restricted,
            vec![Member {
                pubkey: custom_program,
                flags: 0,
            }],
        );

        let message = versioned_message(
            vec![custom_program, restricted],
            vec![CompiledInstruction {
                program_id_index: 0,
                accounts: vec![1],
                data: vec![],
            }],
        );

        assert!(check_versioned_message_admission(&accounts, &message).is_ok());
    }

    #[test]
    fn admission_allows_unrestricted_accounts() {
        let accounts = MockAccountsBank::default();
        let custom_program = Pubkey::new_unique();
        let unrestricted = Pubkey::new_unique();

        let message = versioned_message(
            vec![custom_program, unrestricted],
            vec![CompiledInstruction {
                program_id_index: 0,
                accounts: vec![1],
                data: vec![],
            }],
        );

        assert!(check_versioned_message_admission(&accounts, &message).is_ok());
    }

    #[test]
    fn filter_redacts_full_transaction_when_account_is_invisible() {
        let mut accounts = MockAccountsBank::default();
        let restricted = Pubkey::new_unique();
        let denied = Pubkey::new_unique();
        accounts.insert_permission(restricted, vec![]);

        let original_blockhash = Hash::new_unique();
        let meta = TransactionStatusMeta {
            fee: 5_000,
            pre_balances: vec![1_000],
            post_balances: vec![995],
            log_messages: Some(vec!["secret".to_string()]),
            ..TransactionStatusMeta::default()
        };

        let mut tx = complete_tx(vec![restricted], meta);
        tx.transaction
            .message
            .set_recent_blockhash(original_blockhash);

        let mut confirmed = ConfirmedTransactionWithStatusMeta {
            slot: 0,
            block_time: None,
            tx_with_meta: TransactionWithStatusMeta::Complete(tx),
        };
        filter_confirmed_transaction(&accounts, &mut confirmed, &denied)
            .unwrap();

        let TransactionWithStatusMeta::Complete(filtered) =
            confirmed.tx_with_meta
        else {
            panic!("expected complete tx");
        };
        assert!(filtered
            .transaction
            .message
            .static_account_keys()
            .is_empty());
        assert_eq!(
            filtered.transaction.message.recent_blockhash(),
            &original_blockhash
        );
        assert_eq!(filtered.meta.fee, 0);
        assert!(filtered.meta.pre_balances.is_empty());
        assert!(filtered.meta.log_messages.is_none());
    }

    #[test]
    fn filter_preserves_full_transaction_when_user_has_all_access() {
        let mut accounts = MockAccountsBank::default();
        let restricted = Pubkey::new_unique();
        let allowed = Pubkey::new_unique();
        accounts.insert_permission(
            restricted,
            vec![Member {
                pubkey: allowed,
                flags: u8::MAX,
            }],
        );

        let meta = TransactionStatusMeta {
            pre_balances: vec![1_000],
            post_balances: vec![995],
            log_messages: Some(vec!["allowed".to_string()]),
            ..TransactionStatusMeta::default()
        };

        let mut confirmed = ConfirmedTransactionWithStatusMeta {
            slot: 0,
            block_time: None,
            tx_with_meta: TransactionWithStatusMeta::Complete(complete_tx(
                vec![restricted],
                meta,
            )),
        };
        filter_confirmed_transaction(&accounts, &mut confirmed, &allowed)
            .unwrap();

        let TransactionWithStatusMeta::Complete(filtered) =
            confirmed.tx_with_meta
        else {
            panic!("expected complete tx");
        };
        assert_eq!(
            filtered.transaction.message.static_account_keys(),
            &[restricted]
        );
        assert_eq!(filtered.meta.pre_balances, vec![1_000]);
        assert_eq!(
            filtered.meta.log_messages,
            Some(vec!["allowed".to_string()])
        );
    }

    #[test]
    fn filter_redacts_only_logs_when_only_logs_flag_missing() {
        let mut accounts = MockAccountsBank::default();
        let restricted = Pubkey::new_unique();
        let viewer = Pubkey::new_unique();
        accounts.insert_permission(
            restricted,
            vec![Member {
                pubkey: viewer,
                flags: FLAG_BALANCES | FLAG_MESSAGE,
            }],
        );

        let meta = TransactionStatusMeta {
            pre_balances: vec![1_000],
            post_balances: vec![1_000],
            log_messages: Some(vec!["secret".to_string()]),
            ..TransactionStatusMeta::default()
        };

        let mut confirmed = ConfirmedTransactionWithStatusMeta {
            slot: 0,
            block_time: None,
            tx_with_meta: TransactionWithStatusMeta::Complete(complete_tx(
                vec![restricted],
                meta,
            )),
        };
        filter_confirmed_transaction(&accounts, &mut confirmed, &viewer)
            .unwrap();

        let TransactionWithStatusMeta::Complete(filtered) =
            confirmed.tx_with_meta
        else {
            panic!("expected complete tx");
        };
        assert_eq!(filtered.meta.pre_balances, vec![1_000]);
        assert_eq!(filtered.meta.log_messages, Some(vec![]));
        assert_eq!(
            filtered.transaction.message.static_account_keys(),
            &[restricted]
        );
    }

    #[test]
    fn filter_zeros_balances_when_balance_flag_missing() {
        let mut accounts = MockAccountsBank::default();
        let restricted = Pubkey::new_unique();
        let viewer = Pubkey::new_unique();
        accounts.insert_permission(
            restricted,
            vec![Member {
                pubkey: viewer,
                flags: FLAG_LOGS | FLAG_MESSAGE,
            }],
        );

        let meta = TransactionStatusMeta {
            pre_balances: vec![1_000],
            post_balances: vec![995],
            log_messages: Some(vec!["ok".to_string()]),
            ..TransactionStatusMeta::default()
        };

        let mut confirmed = ConfirmedTransactionWithStatusMeta {
            slot: 0,
            block_time: None,
            tx_with_meta: TransactionWithStatusMeta::Complete(complete_tx(
                vec![restricted],
                meta,
            )),
        };
        filter_confirmed_transaction(&accounts, &mut confirmed, &viewer)
            .unwrap();

        let TransactionWithStatusMeta::Complete(filtered) =
            confirmed.tx_with_meta
        else {
            panic!("expected complete tx");
        };
        assert_eq!(filtered.meta.pre_balances, vec![0]);
        assert_eq!(filtered.meta.post_balances, vec![0]);
        assert_eq!(filtered.meta.log_messages, Some(vec!["ok".to_string()]));
    }
}
