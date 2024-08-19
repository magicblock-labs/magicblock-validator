use solana_sdk::account::{AccountSharedData, WritableAccount};
use solana_sdk::pubkey::Pubkey;
use std::cell::RefCell;

use super::DELEGATION_PROGRAM_ID;

pub(crate) fn set_account_owner(
    acc: &RefCell<AccountSharedData>,
    pubkey: Pubkey,
) {
    acc.borrow_mut().set_owner(pubkey);
}

pub(crate) fn set_account_owner_to_delegation_program(
    acc: &RefCell<AccountSharedData>,
) {
    set_account_owner(acc, *DELEGATION_PROGRAM_ID);
}
