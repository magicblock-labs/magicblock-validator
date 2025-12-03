// CLI Default Values
pub const DEFAULT_REMOTE: &str = "devnet";
pub const DEFAULT_RPC_ADDR: &str = "127.0.0.1:8899";

// Struct Default Values
pub const DEFAULT_STORAGE_DIRECTORY: &str = "magicblock-test-storage/";

// WARNING: This keypair is for development/testing only. Production deployments
// MUST provide their own keypair via config file, env var or CLI argument
pub const DEFAULT_VALIDATOR_KEYPAIR: &str =
    "9Vo7TbA5YfC5a33JhAi9Fb41usA6JwecHNRw3f9MzzHAM8hFnXTzL5DcEHwsAFjuUZ8vNQcJ4XziRFpMc3gTgBQ";
pub const DEFAULT_BASE_FEE: u64 = 100;
pub const DEFAULT_COMPUTE_UNIT_PRICE: u64 = 1_000_000;

// Remote URL Aliases
pub const MAINNET_URL: &str = "https://api.mainnet-beta.solana.com/";
pub const DEVNET_URL: &str = "https://api.devnet.solana.com/";
pub const TESTNET_URL: &str = "https://api.testnet.solana.com/";
pub const LOCALHOST_URL: &str = "http://127.0.0.1:8899/";

// Figment Configuration
pub const ENV_VAR_PREFIX: &str = "MBV_";

// Accounts DB Defaults
pub const DEFAULT_ACCOUNTS_DB_SIZE: usize = 100 * 1024 * 1024; // 100 MB
pub const DEFAULT_ACCOUNTS_INDEX_SIZE: usize = 16 * 1024 * 1024; // 16 MB
pub const DEFAULT_ACCOUNTS_MAX_SNAPSHOTS: u16 = 4;
pub const DEFAULT_ACCOUNTS_SNAPSHOT_FREQUENCY: u64 = 1024;

// Ledger Defaults
pub const DEFAULT_LEDGER_BLOCK_TIME_MS: u64 = 50;
pub const DEFAULT_LEDGER_SIZE: u64 = 100 * 1024 * 1024 * 1024; // 100 GB

// Metrics Defaults
pub const DEFAULT_METRICS_ADDR: &str = "0.0.0.0:9000";
pub const DEFAULT_METRICS_COLLECT_FREQUENCY_SEC: u64 = 30;

// Task Scheduler Defaults
pub const DEFAULT_TASK_SCHEDULER_MIN_FREQUENCY_MILLIS: i64 = 10;
