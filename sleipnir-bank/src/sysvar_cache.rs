// NOTE: copied from bank/sysvar_cache.rs and tests removed
use solana_program_runtime::sysvar_cache::SysvarCache;
use solana_sdk::account::ReadableAccount;

use super::bank::Bank;

impl Bank {
    pub(crate) fn fill_missing_sysvar_cache_entries(&self) {
        let tx_processor = self.transaction_processor.read().unwrap();
        let mut sysvar_cache = tx_processor.sysvar_cache.write().unwrap();
        sysvar_cache.fill_missing_entries(|pubkey, callback| {
            if let Some(account) = self.get_account_with_fixed_root(pubkey) {
                callback(account.data());
            }
        });
    }

    #[allow(dead_code)]
    pub(crate) fn reset_sysvar_cache(&self) {
        let tx_processor = self.transaction_processor.read().unwrap();
        let mut sysvar_cache = tx_processor.sysvar_cache.write().unwrap();
        sysvar_cache.reset();
    }

    pub fn get_sysvar_cache_for_tests(&self) -> SysvarCache {
        self.transaction_processor
            .read()
            .unwrap()
            .sysvar_cache
            .read()
            .unwrap()
            .clone()
    }
}
