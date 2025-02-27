use std::time::{SystemTime, UNIX_EPOCH};

use magicblock_program::Pubkey;
use solana_sdk::{
    account::{
        AccountSharedData, InheritableAccountFields, ReadableAccount,
        WritableAccount,
    },
    clock::INITIAL_RENT_EPOCH,
    sysvar::Sysvar,
};

/// Compute how much an account has changed size.  This function is useful when the data size delta
/// needs to be computed and passed to an `update_accounts_data_size_delta` function.
pub(super) fn calculate_data_size_delta(
    old_data_size: usize,
    new_data_size: usize,
) -> i64 {
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

#[allow(dead_code)] // will need this for millisecond clock
pub fn get_epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

#[allow(dead_code)] // needed when double checking clock calculation
pub(crate) fn get_sys_time_in_secs() -> i64 {
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(n) => {
            let secs = n.as_secs();
            i64::try_from(secs).expect("SystemTime greater i64::MAX")
        }
        Err(_) => panic!("SystemTime before UNIX EPOCH!"),
    }
}

/// Update account data in place if possible.
///
/// This is a performance optimization leveraging
/// the fact that most likely the account will be
/// of AccountSharedData::Borrowed variant and we
/// can modify it inplace instead of cloning things
/// all over the place with extra allocations
pub(crate) fn update_sysvar_data<S: Sysvar>(
    sysvar: &S,
    mut account: Option<AccountSharedData>,
) -> AccountSharedData {
    let sysvar_id =
        Pubkey::from_str_const("Sysvar1111111111111111111111111111111111111");
    let data_len = bincode::serialized_size(sysvar).unwrap() as usize;
    let mut account = account.take().unwrap_or_else(|| {
        AccountSharedData::create(1, vec![], sysvar_id, false, u64::MAX)
    });
    account.resize(data_len, 0);
    bincode::serialize_into(account.data_as_mut_slice(), sysvar)
        .inspect_err(|err| {
            log::error!("failed to bincode serialize sysvar: {err}")
        })
        .expect("sysvar data update failed");
    account
}
