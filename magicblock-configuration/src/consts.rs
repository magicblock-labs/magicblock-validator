// CLI Default Values
pub const DEFAULT_REMOTE: &str = "devnet";
pub const DEFAULT_LIFECYCLE: &str = "programs-replica";
pub const DEFAULT_RPC_ADDR: &str = "127.0.0.1:8899";

// Struct Default Values
pub const DEFAULT_VALIDATOR_KEYPAIR: &str =
    "9Vo7TbA5YfC5a33JhAi9Fb41usA6JwecHNRw3f9MzzHAM8hFnXTzL5DcEHwsAFjuUZ8vNQcJ4XziRFpMc3gTgBQ";
pub const DEFAULT_BASE_FEE: u64 = 100;
pub const DEFAULT_BASE_FEE_STR: &str = "100";
pub const DEFAULT_COMPUTE_UNIT_PRICE: u64 = 1_000_000;

// Remote URL Aliases
pub const MAINNET_URL: &str = "https://api.mainnet-beta.solana.com";
pub const DEVNET_URL: &str = "https://api.devnet.solana.com";
pub const TESTNET_URL: &str = "https://api.testnet.solana.com";
pub const LOCALHOST_URL: &str = "http://127.0.0.1:8899";

// Figment Configuration
pub const ENV_VAR_PREFIX: &str = "MBV_";
