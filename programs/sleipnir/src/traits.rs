use std::collections::HashSet;

use solana_sdk::pubkey::Pubkey;

#[derive(Clone)]
pub enum AccountRemovalReason {
    Undelegated,
}

pub trait AccountsRemover: Clone + Send + Sync {
    fn request_accounts_removal(
        &self,
        pubkey: HashSet<Pubkey>,
        reason: AccountRemovalReason,
    );
}
