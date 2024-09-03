use std::collections::HashSet;

use sleipnir_program::traits::{AccountRemovalReason, AccountsRemover};
use solana_sdk::pubkey::Pubkey;

#[derive(Clone)]
pub struct AccountsRemoverStub;

// TODO(vbrunet) - we can remove all account removal code now
impl AccountsRemover for AccountsRemoverStub {
    fn request_accounts_removal(
        &self,
        _pubkey: HashSet<Pubkey>,
        _reason: AccountRemovalReason,
    ) {
        unimplemented!("AccountsRemoverStub::request_accounts_removal not expected to be called during tests")
    }

    fn accounts_pending_removal(&self) -> HashSet<Pubkey> {
        unimplemented!("AccountsRemoverStub::accounts_pending_removal not expected to be called during tests")
    }
}
