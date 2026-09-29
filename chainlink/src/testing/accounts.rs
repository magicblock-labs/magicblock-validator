use solana_account::{Account, AccountBuilder, AccountMode, AccountSharedData};
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
    AccountBuilder::from(account_shared_with_owner(acc, owner))
        .slot(slot)
        .build()
}

pub fn delegated_account_shared_with_owner(
    acc: &Account,
    owner: Pubkey,
) -> AccountSharedData {
    AccountBuilder::from(account_shared_with_owner(acc, owner))
        .mode(AccountMode::Delegated)
        .build()
}

pub fn account_with_owner(acc: &Account, owner: Pubkey) -> Account {
    let account: AccountSharedData =
        AccountBuilder::from(acc.clone()).owner(owner).build();
    account.into()
}

pub fn delegated_account_shared_with_owner_and_slot(
    acc: &Account,
    owner: Pubkey,
    remote_slot: u64,
) -> AccountSharedData {
    AccountBuilder::from(account_shared_with_owner(acc, owner))
        .mode(AccountMode::Delegated)
        .slot(remote_slot)
        .build()
}
