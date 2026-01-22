use solana_account::AccountSharedData;
use solana_pubkey::Pubkey;

pub trait AccountsBank: Send + Sync + 'static {
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData>;
    fn remove_account(&self, pubkey: &Pubkey);
    fn remove_where(
        &self,
        predicate: impl Fn(&Pubkey, &AccountSharedData) -> bool,
    ) -> usize;

    fn remove_account_conditionally(
        &self,
        pubkey: &Pubkey,
        predicate: impl Fn(&AccountSharedData) -> bool,
    ) {
        if let Some(acc) = self.get_account(pubkey) {
            if predicate(&acc) {
                self.remove_account(pubkey);
            }
        }
    }
}
