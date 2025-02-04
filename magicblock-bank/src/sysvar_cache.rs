// NOTE: copied from bank/sysvar_cache.rs and tests removed
use solana_program_runtime::sysvar_cache::SysvarCache;
use solana_sdk::{account::ReadableAccount, clock::Clock};

use super::bank::Bank;

impl Bank {
    pub(crate) fn fill_missing_sysvar_cache_entries(&self) {
        let tx_processor = self.transaction_processor.read().unwrap();
        tx_processor.fill_missing_sysvar_cache_entries(self);
    }

    pub(crate) fn set_clock_in_sysvar_cache(&self, clock: Clock) {
        let tx_processor = self.transaction_processor.read().unwrap();
        // TODO!!!: we need to store clock somehow
        //tx_processor.sysvar_cache.write().unwrap().set_clock(clock);
    }

    pub fn get_sysvar_cache_for_tests(&self) -> SysvarCache {
        self.transaction_processor
            .read()
            .unwrap()
            .sysvar_cache()
            .clone()
    }
}
