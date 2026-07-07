pub const MAX_WRITE_BUFFER_SIZE: u64 = 256 * 1024 * 1024; // 256MB

// Default capacity of the block cache shared by all column families, sized
// for local development; production nodes should configure much more via
// the `ledger.block-cache-size` setting. With direct reads enabled the OS
// page cache is bypassed, so this is the only read caching layer.
pub const BLOCK_CACHE_CAPACITY: usize = 512 * 1024 * 1024; // 512MB
