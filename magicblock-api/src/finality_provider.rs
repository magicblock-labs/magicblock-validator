use std::sync::Arc;

use magicblock_bank::bank::Bank;
use magicblock_ledger::ledger_purgatory::FinalityProvider;

#[derive(Clone)]
pub struct FinalityProviderImpl {
    pub bank: Arc<Bank>,
}

impl FinalityProvider for FinalityProviderImpl {
    fn get_latest_final_slot(&self) -> u64 {
        self.bank.get_latest_snapshot_slot()
    }
}

impl FinalityProviderImpl {
    pub fn new(bank: Arc<Bank>) -> Self {
        Self { bank }
    }
}
