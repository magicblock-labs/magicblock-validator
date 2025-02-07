pub use solana_accounts_db::account_storage::meta::StoredAccountMeta;
use solana_accounts_db::{
    accounts_db::AccountStorageEntry, accounts_file::AccountsFile,
};

pub fn all_accounts<R>(
    storage: &AccountStorageEntry,
    cb: impl Fn(StoredAccountMeta) -> R,
) -> Vec<R> {
    let av = match &storage.accounts {
        AccountsFile::AppendVec(av) => av,
        AccountsFile::TieredStorage(_) => {
            unreachable!("we never use tiered accounts storage")
        }
    };
    let mut offset = 0;
    let mut accounts = vec![];
    while let Some((account, next)) =
        av.get_stored_account_meta_callback(offset, |a| {
            let offset = a.offset() + a.stored_size();
            (cb(a), offset)
        })
    {
        accounts.push(account);
        offset = next;
    }
    accounts
}

pub fn find_account<R>(
    storage: &AccountStorageEntry,
    cb: impl Fn(StoredAccountMeta) -> Option<R>,
) -> Option<R> {
    let av = match &storage.accounts {
        AccountsFile::AppendVec(av) => av,
        AccountsFile::TieredStorage(_) => {
            unreachable!("we never use tiered accounts storage")
        }
    };
    let mut offset = 0;
    let mut account = None;
    while let Some(false) = av.get_stored_account_meta_callback(offset, |a| {
        offset = a.offset() + a.stored_size();
        if let Some(a) = cb(a) {
            account.replace(a);
            true
        } else {
            false
        }
    }) {} // ugly?
    account
}
