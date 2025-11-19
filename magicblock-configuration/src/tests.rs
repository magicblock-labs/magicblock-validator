use crate::{consts, MagicBlockParams, RemoteCluster};
use serial_test::{parallel, serial};
use std::ffi::OsString;
use std::fs::File;
use std::io::Write;
use std::str::FromStr;
use tempfile::TempDir;

/// Helper to simulate CLI arguments.
/// passing `vec![]` simulates running the binary with no args.
fn run_cli(args: Vec<&str>) -> MagicBlockParams {
    // clap expects the first argument to be the binary name
    let itr = std::iter::once("mb-validator")
        .chain(args.into_iter())
        .map(OsString::from);

    MagicBlockParams::try_new(itr).expect("Failed to parse configuration")
}

struct EnvVarGuard(&'static str);

impl EnvVarGuard {
    fn new(var: &'static str, val: &str) -> Self {
        std::env::set_var(var, val);
        Self(var)
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        std::env::remove_var(self.0);
    }
}

// ============================================================================
// 1: Defaults & Basic CLI
// ============================================================================

#[test]
#[parallel]
fn test_defaults_are_applied() {
    let config = run_cli(vec![]);

    // Check a few key defaults from consts.rs
    assert_eq!(config.validator.basefee, consts::DEFAULT_BASE_FEE);
    assert_eq!(
        config.remote,
        RemoteCluster::from_str(consts::DEFAULT_REMOTE).unwrap()
    );
    assert_eq!(config.listen.0.port(), 8899);
}

#[test]
#[parallel]
fn test_cli_args_override_defaults() {
    // Override basefee via CLI
    let config = run_cli(vec!["--basefee", "999"]);

    assert_eq!(config.validator.basefee, 999);
    // Ensure other defaults remain untouched
    assert_eq!(config.listen.0.port(), 8899);
}

// ============================================================================
// 2: Precedence Logic
// Order: Env Var > TOML File > CLI > Defaults
// ============================================================================

/// Helper to create a temporary config file
fn create_temp_config(content: &str) -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("magicblock.toml");
    let mut file = File::create(&file_path).unwrap();
    writeln!(&mut file, "{}", content).unwrap();
    (dir, file_path)
}

#[test]
#[serial]
fn test_env_var_overrides_cli() {
    // 1. Set an environment variable
    let _env = EnvVarGuard::new("MBV_VALIDATOR_BASEFEE", "5000");

    // 2. Pass a conflicting CLI argument (e.g., 100)
    // Even though CLI says 100, Env says 5000. Env should win.
    let config = run_cli(vec!["--basefee", "100"]);

    assert_eq!(config.validator.basefee, 5000);
}

#[test]
#[parallel]
fn test_toml_file_overrides_cli() {
    // 1. Create a config file specifying basefee = 200
    let (_dir, config_path) = create_temp_config(
        r#"
        [validator]
        basefee = 200
        "#,
    );

    // 2. Run CLI with --config pointing to file, and a conflicting --basefee 999
    // Logic: TOML (200) > CLI (999)
    let config = run_cli(vec![
        "--config",
        config_path.to_str().unwrap(),
        "--basefee",
        "999",
    ]);

    assert_eq!(config.validator.basefee, 200);
}

#[test]
#[serial]
fn test_env_overrides_toml() {
    // 1. Create TOML with fee 300
    let (_dir, config_path) = create_temp_config(
        r#"
        [validator]
        basefee = 300
        "#,
    );

    // 2. Set Env with fee 400
    let _env = EnvVarGuard::new("MBV_VALIDATOR_BASEFEE", "400");

    // 3. Run
    let config = run_cli(vec!["--config", config_path.to_str().unwrap()]);

    // Env (400) > TOML (300)
    assert_eq!(config.validator.basefee, 400);
}

use crate::types::Remote;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

// ============================================================================
// 3: Custom Type Parsing
// ============================================================================

#[test]
#[parallel]
fn test_remote_parsing_aliases() {
    // 1. Test "devnet" alias expansion
    let config = run_cli(vec!["--remote", "devnet"]);

    // Verify it expanded to the full URL defined in consts.rs
    match config.remote {
        RemoteCluster::Single(Remote::Unified(url)) => {
            assert_eq!(url.0.as_str(), crate::consts::DEVNET_URL);
        }
        _ => panic!("Expected Single Unified remote for alias 'devnet'"),
    }
}

#[test]
#[parallel]
fn test_remote_parsing_custom_url() {
    // 2. Test custom URL pass-through
    let custom_url = "http://my-private-rpc.com:8899/";
    let config = run_cli(vec!["--remote", custom_url]);

    match config.remote {
        RemoteCluster::Single(Remote::Unified(url)) => {
            assert_eq!(url.0.as_str(), custom_url);
        }
        _ => panic!("Expected Single Unified remote for custom URL"),
    }
}

#[test]
#[parallel]
fn test_bind_address_parsing() {
    // 1. Test valid IPv4 and port
    let config = run_cli(vec!["--listen", "0.0.0.0:9090"]);

    assert_eq!(
        config.listen.0,
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 9090)
    );
}

use crate::{config::BlockSize, LifecycleMode};

// ============================================================================
// 4: Complex Mixed Precedence & Serialization
// ============================================================================

#[test]
#[serial]
fn test_mixed_precedence_all_sources() {
    // Scenario:
    // 1. FILE: Sets `storage` path and `accounts-db` size.
    // 2. ENV:  Sets `validator.basefee` (Overriding defaults).
    // 3. CLI:  Sets `listen` address and `lifecycle` mode.
    // 4. DEFAULT: `ledger.block_time` remains untouched.

    // 1. Setup File
    let (_dir, config_path) = create_temp_config(
        r#"
        storage = "/tmp/magicblock-data"
        
        [accounts-db]
        database-size = 524288000 # 500 MB
        block-size = "block512"
        "#,
    );

    // 2. Setup Env
    let _env = EnvVarGuard::new("MBV_VALIDATOR_BASEFEE", "777");

    // 3. Run CLI
    let config = run_cli(vec![
        "--config",
        config_path.to_str().unwrap(),
        "--listen",
        "127.0.0.1:5000",
        "--lifecycle",
        "offline",
    ]);

    // --- Assertions ---

    // From CLI
    assert_eq!(config.listen.0.port(), 5000);
    assert_eq!(config.lifecycle, LifecycleMode::Offline);

    // From Env
    assert_eq!(config.validator.basefee, 777);

    // From File
    assert_eq!(
        config.storage.unwrap().to_str().unwrap(),
        "/tmp/magicblock-data"
    );
    assert_eq!(config.accounts_db.database_size, 524_288_000);

    // Check Enum parsing from string in file
    assert!(matches!(config.accounts_db.block_size, BlockSize::Block512));

    // From Default (Untouched)
    assert_eq!(config.ledger.block_time.as_millis(), 400);
}
