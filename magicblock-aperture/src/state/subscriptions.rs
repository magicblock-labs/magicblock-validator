use std::sync::atomic::{AtomicU64, Ordering};

/// A unique identifier for a single subscription, returned to the client.
pub(crate) type SubscriptionID = u64;

/// A global atomic counter for generating unique subscription IDs.
static SUBID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generates the next unique subscription ID.
pub(crate) fn next_subid() -> SubscriptionID {
    SUBID_COUNTER.fetch_add(1, Ordering::Relaxed)
}
