use std::collections::HashSet;

use solana_sdk::pubkey::Pubkey;

#[derive(Clone)]
pub enum AccountRemovalReason {
    Undelegated,
}

pub trait AccountsRemover: Clone + Send + Sync + 'static {
    fn request_accounts_removal(
        &self,
        pubkey: HashSet<Pubkey>,
        reason: AccountRemovalReason,
    );

    fn has_accounts_pending_removal(&self) -> bool;
}
