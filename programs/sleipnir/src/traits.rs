use std::collections::HashSet;

use solana_sdk::pubkey::Pubkey;

#[derive(Clone, Debug)]
pub enum AccountRemovalReason {
    Undelegated,
}

pub trait AccountsRemover: Clone + Send + Sync + 'static {
    fn request_accounts_removal(
        &self,
        pubkey: HashSet<Pubkey>,
        reason: AccountRemovalReason,
    );

    fn accounts_pending_removal(&self) -> HashSet<Pubkey>;
}
