use std::num::Saturating;

// -----------------
// StoreAccountsTiming
// -----------------
#[derive(Default, Debug)]
pub struct StoreAccountsTiming {
    pub store_accounts_elapsed: u64,
    pub update_index_elapsed: u64,
    pub handle_reclaims_elapsed: u64,
}

impl StoreAccountsTiming {
    pub fn accumulate(&mut self, other: &Self) {
        self.store_accounts_elapsed += other.store_accounts_elapsed;
        self.update_index_elapsed += other.update_index_elapsed;
        self.handle_reclaims_elapsed += other.handle_reclaims_elapsed;
    }
}

// -----------------
// FlushStats
// -----------------
#[derive(Debug, Default)]
pub struct FlushStats {
    pub num_flushed: Saturating<usize>,
    pub num_purged: Saturating<usize>,
    pub total_size: Saturating<u64>,
    pub store_accounts_timing: StoreAccountsTiming,
    pub store_accounts_total_us: Saturating<u64>,
}

impl FlushStats {
    pub fn accumulate(&mut self, other: &Self) {
        self.num_flushed += other.num_flushed;
        self.num_purged += other.num_purged;
        self.total_size += other.total_size;
        self.store_accounts_timing
            .accumulate(&other.store_accounts_timing);
        self.store_accounts_total_us += other.store_accounts_total_us;
    }
}
