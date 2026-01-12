/// CLI Default Values
/// Default remote endpoint: devnet HTTP URL
pub const DEFAULT_REMOTE: &str = DEVNET_URL;

/// Default RPC address for the validator service
pub const DEFAULT_RPC_ADDR: &str = "127.0.0.1:8899";

/// Struct Default Values
/// Default storage directory for ledger and accounts data
pub const DEFAULT_STORAGE_DIRECTORY: &str = "magicblock-test-storage/";

/// WARNING: This keypair is for development/testing only.
/// Production deployments MUST provide their own keypair via config file, env var, or CLI argument.
pub const DEFAULT_VALIDATOR_KEYPAIR: &str =
     "9Vo7TbA5YfC5a33JhAi9Fb41usA6JwecHNRw3f9MzzHAM8hFnXTzL5DcEHwsAFjuUZ8vNQcJ4XziRFpMc3gTgBQ";

/// Default base fee in lamports for transactions
pub const DEFAULT_BASE_FEE: u64 = 0;

/// Default compute unit price in microlamports
pub const DEFAULT_COMPUTE_UNIT_PRICE: u64 = 1_000_000;

/// Remote URL Aliases - Mainnet, Testnet, Devnet, and Localhost
/// Solana mainnet-beta RPC endpoint
pub const MAINNET_URL: &str = "https://api.mainnet-beta.solana.com/";

/// Solana testnet RPC endpoint
pub const TESTNET_URL: &str = "https://api.testnet.solana.com/";

/// Solana devnet RPC endpoint (default for development)
pub const DEVNET_URL: &str = "https://api.devnet.solana.com/";

/// Localhost RPC endpoint for local development
pub const LOCALHOST_URL: &str = "http://localhost:8899/";

/// Figment Configuration
/// Environment variable prefix for configuration (MBV_)
pub const ENV_VAR_PREFIX: &str = "MBV_";

/// Accounts DB Defaults
/// Default size of the accounts database (100 MB)
pub const DEFAULT_ACCOUNTS_DB_SIZE: usize = 100 * 1024 * 1024;

/// Default size of the accounts index (16 MB)
pub const DEFAULT_ACCOUNTS_INDEX_SIZE: usize = 16 * 1024 * 1024;

/// Maximum number of account snapshots to retain
pub const DEFAULT_ACCOUNTS_MAX_SNAPSHOTS: u16 = 4;

/// Frequency of account snapshots (every N slots)
pub const DEFAULT_ACCOUNTS_SNAPSHOT_FREQUENCY: u64 = 1024;

/// Ledger Defaults
/// Default block time in milliseconds
pub const DEFAULT_LEDGER_BLOCK_TIME_MS: u64 = 50;

/// Default ledger size (100 GB)
pub const DEFAULT_LEDGER_SIZE: u64 = 100 * 1024 * 1024 * 1024;

/// Metrics Defaults
/// Default address for the metrics endpoint (Prometheus format)
pub const DEFAULT_METRICS_ADDR: &str = "0.0.0.0:9000";

/// Default frequency of metrics collection in seconds
pub const DEFAULT_METRICS_COLLECT_FREQUENCY_SEC: u64 = 30;

// Task Scheduler Defaults
pub const DEFAULT_TASK_SCHEDULER_MIN_INTERVAL_MILLIS: u64 = 10;

// ChainLink Defaults
/// Default delay in milliseconds between resubscribing to accounts after a pubsub reconnection
pub const DEFAULT_RESUBSCRIPTION_DELAY_MS: u64 = 50;
