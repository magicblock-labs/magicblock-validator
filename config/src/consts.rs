use solana_pubkey::Pubkey;

/// Validator-controlled scratch account used by the RPC healthcheck.
pub const HEALTHCHECK_ACCOUNT_PUBKEY: Pubkey =
    Pubkey::from_str_const("V42Hea1thCheckTargetMagicb1ock1111111111111");

/// CLI Default Values
/// Default remote endpoint: devnet HTTP URL
pub const DEFAULT_REMOTE: &str = DEVNET_URL;

/// Default RPC address for the validator service
pub const DEFAULT_RPC_ADDR: &str = "127.0.0.1:8899";

/// Default durable root for engine and validator state.
pub const DEFAULT_ENGINE_LEDGER_DIRECTORY: &str = "magicblock-test-storage/";

/// WARNING: This keypair is for development/testing only.
/// Production deployments MUST provide their own keypair via config file, env var, or CLI argument.
pub const DEFAULT_VALIDATOR_KEYPAIR: &str =
    "9Vo7TbA5YfC5a33JhAi9Fb41usA6JwecHNRw3f9MzzHAM8hFnXTzL5DcEHwsAFjuUZ8vNQcJ4XziRFpMc3gTgBQ";

/// Default compute unit price in microlamports
pub const DEFAULT_COMPUTE_UNIT_PRICE: u64 = 1_000_000;

/// Solana devnet RPC endpoint (default for development)
pub const DEVNET_URL: &str = "https://api.devnet.solana.com/";

/// Figment Configuration
/// Environment variable prefix for configuration (MBV_)
pub const ENV_VAR_PREFIX: &str = "MBV_";

/// Maximum number of resolved accounts retained for recency and eviction.
pub const DEFAULT_ACCOUNTS_LRU_CAPACITY: usize = 5_000;

/// Ledger Defaults
/// Default block time in milliseconds
pub const DEFAULT_LEDGER_BLOCK_TIME_MS: u64 = 50;

/// Default ledger size (100 GB)
pub const DEFAULT_LEDGER_SIZE: u64 = 100 * 1024 * 1024 * 1024;

/// Default superblock size (72K ~ 1 hour with 50ms block time)
pub const DEFAULT_SUPERBLOCK_SIZE: u64 = 3600 * 20;

/// Default leader replication listener.
pub const DEFAULT_REPLICATION_BIND_ADDRESS: &str = "127.0.0.1:10000";

/// Default ledger block cache size (512 MB, suits local dev;
/// production nodes should configure 16 GB or more)
pub const DEFAULT_LEDGER_BLOCK_CACHE_SIZE: u64 = 512 * 1024 * 1024;

/// Metrics Defaults
/// Default address for the metrics endpoint (Prometheus format)
pub const DEFAULT_METRICS_ADDR: &str = "0.0.0.0:9000";
