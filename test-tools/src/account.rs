use magicblock_accounts_db::AccountsDb;
use solana_sdk::{account::AccountSharedData, pubkey::Pubkey};

pub fn fund_account(accountsdb: &AccountsDb, pubkey: &Pubkey, lamports: u64) {
    let account = AccountSharedData::new(lamports, 0, &Default::default());
    accountsdb.insert_account(pubkey, &account);
}
