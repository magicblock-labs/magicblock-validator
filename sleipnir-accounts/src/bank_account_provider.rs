use sleipnir_bank::bank::Bank;
use solana_sdk::{account::AccountSharedData, pubkey::Pubkey};
use std::sync::Arc;

use crate::InternalAccountProvider;

pub struct BankAccountProvider(Arc<Bank>);

impl BankAccountProvider {
    pub fn new(bank: Arc<Bank>) -> Self {
        Self(bank)
    }
}

impl InternalAccountProvider for BankAccountProvider {
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        self.0.get_account(pubkey)
    }
}
