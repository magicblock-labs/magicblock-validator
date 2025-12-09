use std::cell::RefCell;

use solana_account::{AccountSharedData, WritableAccount};
use solana_pubkey::Pubkey;

use super::DELEGATION_PROGRAM_ID;

pub(crate) fn set_account_owner(
    acc: &RefCell<AccountSharedData>,
    pubkey: Pubkey,
) {
    acc.borrow_mut().set_owner(pubkey);
}

/// Sets proper account values during undelegation
pub(crate) fn mark_account_as_undelegated(acc: &RefCell<AccountSharedData>) {
    set_account_owner(acc, DELEGATION_PROGRAM_ID);
    let mut acc = acc.borrow_mut();
    acc.set_undelegating(true);
    acc.set_delegated(false);
}
