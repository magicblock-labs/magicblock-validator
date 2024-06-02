/// The maximum number our accounts cache can hold to perform scans synchronously.
/// If it is exceeded, we handle it in parallel across our threadpool.
pub(crate) const SCAN_SLOT_PAR_ITER_THRESHOLD: usize = 4000;
