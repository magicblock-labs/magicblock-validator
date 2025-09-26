use std::cell::RefCell;

use solana_sdk::{
    account::{AccountSharedData, WritableAccount},
    pubkey::Pubkey,
};

use super::DELEGATION_PROGRAM_ID;

pub(crate) fn set_account_owner(
    acc: &RefCell<AccountSharedData>,
    pubkey: Pubkey,
) {
    acc.borrow_mut().set_owner(pubkey);
}

/// Sets proper values on account during undelegation
pub(crate) fn set_account_owner_to_delegation_program(
    acc: &RefCell<AccountSharedData>,
) {
    set_account_owner(acc, DELEGATION_PROGRAM_ID);
}
