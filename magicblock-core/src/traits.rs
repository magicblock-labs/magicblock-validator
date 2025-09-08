use std::{error::Error, fmt};

use solana_account::AccountSharedData;
use solana_pubkey::Pubkey;

pub trait PersistsAccountModData: Sync + Send + fmt::Display + 'static {
    fn persist(&self, id: u64, data: Vec<u8>) -> Result<(), Box<dyn Error>>;
    fn load(&self, id: u64) -> Result<Option<Vec<u8>>, Box<dyn Error>>;
}

pub trait AccountsBank: Send + Sync + 'static {
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData>;
    fn remove_account(&self, pubkey: &Pubkey);
}
