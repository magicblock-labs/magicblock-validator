use rocksdb::Options;

use super::options::AccessType;

pub fn get_rocksdb_options(access_type: &AccessType) -> Options {
    let mut options = Options::default();

    // Create missing items to support a clean start
    options.create_if_missing(true);
    options.create_missing_column_families(true);

    // Background thread prioritization: give flushes more threads, limit compaction threads (low-priority)
    let mut env = rocksdb::Env::new().unwrap();
    let cpus_env = num_cpus::get() as i32;

    // Bottom-priority are used for bottommost compactions. Keep it minimal - 1
    let bottom_pri = 1;
    env.set_bottom_priority_background_threads(bottom_pri);
    // High-priority threads are used for flush. Keep a few to avoid memtable flush backlog.
    let high_pri = cpus_env.clamp(2, 4);
    env.set_high_priority_background_threads(high_pri);
    options.set_env(&env);

    // For every job RocksDB picks a thread from available pools
    // Here we select ceiling so RocksDB doesn't use all of HIGH + LOW + BOTTOM threads
    // By default num of threads in LOW is 1
    let max_jobs = std::cmp::max(high_pri + 1, bottom_pri);
    options.set_max_background_jobs(max_jobs);
    
    // Bound WAL size
    options.set_max_total_wal_size(4 * 1024 * 1024 * 1024);

    if should_disable_auto_compactions(access_type) {
        options.set_disable_auto_compactions(true);
    }

    // Allow Rocks to open/keep open as many files as it needs for performance;
    // however, this is also explicitly required for a secondary instance.
    // See https://github.com/facebook/rocksdb/wiki/Secondary-instance
    options.set_max_open_files(-1);

    // Smooth IO
    options.set_bytes_per_sync(1024 * 1024);
    options.set_wal_bytes_per_sync(1024 * 1024);

    // Favor concurrency on the write path
    options.set_allow_concurrent_memtable_write(true);
    options.set_enable_pipelined_write(true);
    options.set_enable_write_thread_adaptive_yield(true);

    // Use direct IO for compaction/flush to avoid page cache contention
    options.set_use_direct_reads(true);
    options.set_use_direct_io_for_flush_and_compaction(true);
    options.set_compaction_readahead_size(4 * 1024 * 1024);

    // Throttle background compaction/flush IO to avoid starving foreground ops
    // RateLimiter parameters: rate_bytes_per_sec, refill_period_us, fairness
    // Start with a conservative 128 MiB/s, adjustable via config later if needed
    options.set_ratelimiter(128 * 1024 * 1024, 100 * 1000, 10);

    // Dynamic level bytes is a good default to balance levels
    options.set_level_compaction_dynamic_level_bytes(true);
    options.set_report_bg_io_stats(true);

    options
}

// Returns whether automatic compactions should be disabled for the entire
// database based upon the given access type.
pub fn should_disable_auto_compactions(access_type: &AccessType) -> bool {
    // Leave automatic compactions enabled (do not disable) in Primary mode;
    // disable in all other modes to prevent accidental cleaning
    !matches!(access_type, AccessType::Primary)
}
