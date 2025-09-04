#![allow(dead_code)]
use magicblock_chainlink::testing::accounts::account_shared_with_owner;
use solana_account::{Account, AccountSharedData};
use solana_pubkey::Pubkey;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    transaction::{SanitizedTransaction, Transaction},
};

pub fn account_shared_with_owner_and_slot(
    acc: &Account,
    owner: Pubkey,
    slot: u64,
) -> AccountSharedData {
    let mut acc = account_shared_with_owner(acc, owner);
    acc.set_remote_slot(slot);
    acc
}

#[derive(Debug, Clone)]
pub struct TransactionAccounts {
    pub readonly_accounts: Vec<Pubkey>,
    pub writable_accounts: Vec<Pubkey>,
    pub programs: Vec<Pubkey>,
}

impl Default for TransactionAccounts {
    fn default() -> Self {
        Self {
            readonly_accounts: Default::default(),
            writable_accounts: Default::default(),
            programs: vec![solana_sdk::system_program::id()],
        }
    }
}

impl TransactionAccounts {
    pub fn all_sorted(&self) -> Vec<Pubkey> {
        let mut vec = self
            .readonly_accounts
            .iter()
            .chain(self.writable_accounts.iter())
            .chain(self.programs.iter())
            .cloned()
            .collect::<Vec<_>>();
        vec.sort();
        vec
    }
}

pub fn sanitized_transaction_with_accounts(
    transaction_accounts: &TransactionAccounts,
) -> SanitizedTransaction {
    let TransactionAccounts {
        readonly_accounts,
        writable_accounts,
        programs,
    } = transaction_accounts;
    let ix = Instruction::new_with_bytes(
        programs[0],
        &[],
        readonly_accounts
            .iter()
            .map(|k| AccountMeta::new_readonly(*k, false))
            .chain(
                writable_accounts
                    .iter()
                    .enumerate()
                    .map(|(idx, k)| AccountMeta::new(*k, idx == 0)),
            )
            .collect::<Vec<_>>(),
    );
    let mut ixs = vec![ix];
    for program in programs.iter().skip(1) {
        let ix = Instruction::new_with_bytes(*program, &[], vec![]);
        ixs.push(ix);
    }
    SanitizedTransaction::from_transaction_for_tests(Transaction::new_unsigned(
        solana_sdk::message::Message::new(&ixs, None),
    ))
}
