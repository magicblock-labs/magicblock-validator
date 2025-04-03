// NOTE: copied from bank/sysvar_cache.rs and tests removed
use solana_program_runtime::sysvar_cache::SysvarCache;
use solana_sdk::clock::Clock;

use super::bank::Bank;

impl Bank {
    pub(crate) fn fill_missing_sysvar_cache_entries(&self) {
        let tx_processor = self.transaction_processor.read().unwrap();
        tx_processor.fill_missing_sysvar_cache_entries(self);
    }

    pub(crate) fn set_clock_in_sysvar_cache(&self, clock: Clock) {
        #[allow(clippy::readonly_write_lock)]
        let tx_processor = self.transaction_processor.write().unwrap();
        // TODO(bmuddha): get rid of this ugly hack after PR merge
        // https://github.com/anza-xyz/agave/pull/5495
        //
        // SAFETY: we cannot get a &mut to inner SysvarCache as it's
        // private and there's no way to set clock variable directly besides
        // the `fill_missing_sysvar_cache_entries` which is quite expensive
        //
        // ugly hack: this is formally a vialotion of rust's aliasing rules (UB),
        // but we have just acquired an exclusive lock, and thus it's guaranteed
        // that no other thread is reading the sysvar_cache, so we can mutate it
        //
        //
        let ptr = (&*tx_processor.sysvar_cache()) as *const SysvarCache
            as *mut SysvarCache;
        #[allow(invalid_reference_casting)]
        unsafe { &mut *ptr }.set_sysvar_for_tests(&clock);
    }
}
