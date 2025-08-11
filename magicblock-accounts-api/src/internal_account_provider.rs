use solana_account::AccountSharedData;
use solana_pubkey::Pubkey;

pub trait InternalAccountProvider: Send + Sync {
    fn has_account(&self, pubkey: &Pubkey) -> bool;
    fn remove_account(&self, _pubkey: &Pubkey) {}
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData>;
    fn get_all_accounts(&self) -> Vec<(Pubkey, AccountSharedData)>;
    fn get_slot(&self) -> u64;
}
