use solana_account::{
    Account, AccountFieldPatch, AccountMode, AccountSharedData, WritableAccount,
};
use solana_pubkey::Pubkey;

pub fn account_shared_with_owner(
    acc: &Account,
    owner: Pubkey,
) -> AccountSharedData {
    let acc = account_with_owner(acc, owner);
    AccountSharedData::from(acc)
}

pub fn account_shared_with_owner_and_slot(
    acc: &Account,
    owner: Pubkey,
    slot: u64,
) -> AccountSharedData {
    let mut acc = account_shared_with_owner(acc, owner);
    AccountFieldPatch::Slot(slot).apply(&mut acc);
    acc
}

pub fn delegated_account_shared_with_owner(
    acc: &Account,
    owner: Pubkey,
) -> AccountSharedData {
    let mut acc = account_shared_with_owner(acc, owner);
    acc.set_mode(AccountMode::Delegated);
    acc
}

pub fn account_with_owner(acc: &Account, owner: Pubkey) -> Account {
    let mut acc = acc.clone();
    acc.set_owner(owner);
    acc
}

pub fn delegated_account_shared_with_owner_and_slot(
    acc: &Account,
    owner: Pubkey,
    remote_slot: u64,
) -> AccountSharedData {
    let mut acc = account_shared_with_owner(acc, owner);
    acc.set_mode(AccountMode::Delegated);
    AccountFieldPatch::Slot(remote_slot).apply(&mut acc);
    acc
}
