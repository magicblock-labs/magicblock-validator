use compressed_delegation_client::CompressedDelegationRecord;
use solana_account::{Account, AccountSharedData, WritableAccount};
use solana_pubkey::Pubkey;

pub fn account_shared_with_owner(
    acc: &Account,
    owner: Pubkey,
) -> AccountSharedData {
    let acc = account_with_owner(acc, owner);
    AccountSharedData::from(acc)
}

pub fn delegated_account_shared_with_owner(
    acc: &Account,
    owner: Pubkey,
) -> AccountSharedData {
    let mut acc = account_shared_with_owner(acc, owner);
    acc.set_delegated(true);
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
    acc.set_delegated(true);
    acc.set_remote_slot(remote_slot);
    acc
}

pub fn compressed_account_shared_with_owner_and_slot(
    pda: Pubkey,
    authority: Pubkey,
    delegation_slot: u64,
    account: Account,
) -> AccountSharedData {
    let delegation_record_bytes = borsh::to_vec(&CompressedDelegationRecord {
        pda,
        authority,
        last_update_nonce: 0,
        is_undelegatable: false,
        owner: account.owner,
        delegation_slot,
        lamports: account.lamports,
        data: account.data,
    })
    .unwrap();
    let mut acc = Account::new(
        0,
        delegation_record_bytes.len(),
        &compressed_delegation_client::ID,
    );
    acc.data = delegation_record_bytes;
    let mut acc = AccountSharedData::from(acc);
    acc.set_remote_slot(delegation_slot);
    acc
}
