use solana_sdk::{
    account::{AccountSharedData, InheritableAccountFields, ReadableAccount},
    clock::INITIAL_RENT_EPOCH,
};
use std::time::{SystemTime, UNIX_EPOCH};

/// Compute how much an account has changed size.  This function is useful when the data size delta
/// needs to be computed and passed to an `update_accounts_data_size_delta` function.
pub(super) fn calculate_data_size_delta(old_data_size: usize, new_data_size: usize) -> i64 {
    assert!(old_data_size <= i64::MAX as usize);
    assert!(new_data_size <= i64::MAX as usize);
    let old_data_size = old_data_size as i64;
    let new_data_size = new_data_size as i64;

    new_data_size.saturating_sub(old_data_size)
}

pub(super) fn inherit_specially_retained_account_fields(
    old_account: &Option<AccountSharedData>,
) -> InheritableAccountFields {
    const RENT_UNADJUSTED_INITIAL_BALANCE: u64 = 1;
    (
        old_account
            .as_ref()
            .map(|a| a.lamports())
            .unwrap_or(RENT_UNADJUSTED_INITIAL_BALANCE),
        old_account
            .as_ref()
            .map(|a| a.rent_epoch())
            .unwrap_or(INITIAL_RENT_EPOCH),
    )
}

pub fn get_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

pub fn get_epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}
