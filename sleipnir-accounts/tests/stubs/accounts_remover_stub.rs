use std::collections::HashSet;

use sleipnir_program::traits::{AccountRemovalReason, AccountsRemover};
use solana_sdk::pubkey::Pubkey;

#[derive(Clone)]
pub struct AccountsRemoverStub;

impl AccountsRemover for AccountsRemoverStub {
    fn request_accounts_removal(
        &self,
        _pubkey: HashSet<Pubkey>,
        _reason: AccountRemovalReason,
    ) {
        // TODO @@@
        todo!("AccountsRemoverStub::request_accounts_removal")
    }

    fn has_accounts_pending_removal(&self) -> bool {
        // TODO @@@
        todo!("AccountsRemoverStub::has_accounts_pending_removal")
    }
}
