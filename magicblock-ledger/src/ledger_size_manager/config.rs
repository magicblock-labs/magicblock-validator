use solana_sdk::clock::Slot;

/// Percentage of ledger to keep when resizing.
pub enum ResizePercentage {
    /// Keep 75% of the ledger size.
    Large,
    /// Keep 66% of the ledger size.
    Medium,
    /// Keep 50% of the ledger size.
    Small,
}

impl ResizePercentage {
    /// The portion of ledger we cut on each resize.
    pub fn watermark_size_percent(&self) -> u64 {
        use ResizePercentage::*;
        match self {
            Large => 25,
            Medium => 34,
            Small => 50,
        }
    }

    /// The number of watermarks to track
    pub fn watermark_count(&self) -> u64 {
        use ResizePercentage::*;
        match self {
            Large => 3,
            Medium => 2,
            Small => 1,
        }
    }
}

pub struct LedgerSizeManagerConfig {
    /// Max ledger size to maintain.
    /// The [LedgerSizeManager] will attempt to respect this size,, but
    /// it may grow larger temporarily in between size checks.
    pub max_size: u64,

    /// Interval at which the size is checked in milliseconds.
    pub size_check_interval_ms: u64,

    /// Percentage of the ledger to keep when resizing
    pub resize_percentage: ResizePercentage,
}

pub struct ExistingLedgerState {
    /// The current size of the ledger
    pub(crate) size: u64,
    /// The last slot in the ledger at time of restart
    pub(crate) slot: Slot,
    /// The last account mod ID in the ledger at time of restart
    pub(crate) mod_id: u64,
}
