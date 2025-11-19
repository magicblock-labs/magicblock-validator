use crate::{consts, MagicBlockParams, RemoteCluster};
use serial_test::{parallel, serial};
use std::ffi::OsString;
use std::fs::File;
use std::io::Write;
use std::str::FromStr;
use tempfile::TempDir;

/// Helper to simulate CLI arguments.
fn run_cli(args: Vec<&str>) -> MagicBlockParams {
    // clap expects the first argument to be the binary name
    let itr = std::iter::once("mb-validator")
        .chain(args.into_iter())
        .map(OsString::from);

    MagicBlockParams::try_new(itr).expect("Failed to parse configuration")
}

/// RAII Guard to safely handle Env Vars in tests
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
    // Other defaults remain
    assert_eq!(config.listen.0.port(), 8899);
}

// ============================================================================
// 2: Precedence Logic (Updated)
// Order: CLI > Env Var > TOML File > Defaults
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
fn test_cli_overrides_env_var() {
    // 1. Set Env to 5000
    let _env = EnvVarGuard::new("MBV_STORAGE", "/tmp/powerlog");

    // 2. Set CLI to 100
    // Logic: CLI (100) > Env (5000)
    let config =
        run_cli(vec!["--storage", "/tmp/com.apple.launchd.D1zPwBXAsm"]);

    assert_eq!(
        config.storage,
        Some("/tmp/com.apple.launchd.D1zPwBXAsm".parse().unwrap())
    );
}

#[test]
#[parallel]
fn test_cli_overrides_toml_file() {
    // 1. File says 200
    let (_dir, config_path) = create_temp_config(
        r#"
        [validator]
        basefee = 200
        "#,
    );

    // 2. CLI says 999
    // New Logic: CLI (999) > TOML (200)
    let config = run_cli(vec![
        "--config",
        config_path.to_str().unwrap(),
        "--basefee",
        "999",
    ]);

    assert_eq!(config.validator.basefee, 999);
}

#[test]
#[serial]
fn test_env_overrides_toml() {
    // 1. File says 300
    let (_dir, config_path) = create_temp_config(
        r#"
        [validator]
        basefee = 300
        "#,
    );

    // 2. Env says 400
    let _env = EnvVarGuard::new("MBV_VALIDATOR_BASEFEE", "400");

    // 3. Run (pointing to config)
    let config = run_cli(vec!["--config", config_path.to_str().unwrap()]);

    // Logic: Env (400) > TOML (300)
    // (Because Env is merged AFTER Toml in lib.rs)
    assert_eq!(config.validator.basefee, 400);
}

#[test]
#[serial]
fn test_full_precedence_stack() {
    // Verifies the full chain: CLI > Env > TOML > Default
    // We use `basefee` as the target. Default is 100 (approx).

    let (_dir, config_path) = create_temp_config(
        r#"
            [validator]
            basefee = 1000
        "#,
    );
    let _env = EnvVarGuard::new("MBV_VALIDATOR_BASEFEE", "2000");

    // 1. Check Env > TOML
    let config_no_cli =
        run_cli(vec!["--config", config_path.to_str().unwrap()]);
    assert_eq!(config_no_cli.validator.basefee, 2000);

    // 2. Check CLI > Env
    let config_with_cli = run_cli(vec![
        "--config",
        config_path.to_str().unwrap(),
        "--basefee",
        "3000",
    ]);
    assert_eq!(config_with_cli.validator.basefee, 3000);
}

use crate::types::Remote;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

// ============================================================================
// 3: Custom Type Parsing
// ============================================================================

#[test]
#[parallel]
fn test_remote_parsing_aliases() {
    let config = run_cli(vec!["--remote", "devnet"]);

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
    let config = run_cli(vec!["--listen", "0.0.0.0:9090"]);

    assert_eq!(
        config.listen.0,
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 9090)
    );
}

use crate::{config::BlockSize, LifecycleMode};

// ============================================================================
// 4: Complex Mixed Precedence
// ============================================================================

#[test]
#[serial]
fn test_mixed_precedence_all_sources() {
    // Scenario:
    // 1. FILE: Sets `storage` path, `accounts-db` size, and a `basefee` of 500
    // 2. ENV:  Sets `validator.basefee` to 777 (should override FILE)
    // 3. CLI:  Sets `listen` address and `lifecycle` mode (should overlay)

    let (_dir, config_path) = create_temp_config(
        r#"storage = "/tmp/magicblock-data"
            [validator]
            basefee = 500
            [accounts-db]
            database-size = 524288000
            block-size = "block512"
        "#,
    );

    // Env overrides the File's basefee (500 -> 777)
    let _env = EnvVarGuard::new("MBV_VALIDATOR_BASEFEE", "777");

    // CLI sets orthogonal fields
    let config = run_cli(vec![
        "--config",
        config_path.to_str().unwrap(),
        "--listen",
        "127.0.0.1:5000",
        "--lifecycle",
        "offline",
    ]);

    // CLI fields
    assert_eq!(config.listen.0.port(), 5000);
    assert_eq!(config.lifecycle, LifecycleMode::Offline);

    // Env overrides File
    assert_eq!(config.validator.basefee, 777);

    // File fields (not touched by others)
    assert_eq!(
        config.storage.unwrap().to_str().unwrap(),
        "/tmp/magicblock-data"
    );
    assert_eq!(config.accounts_db.database_size, 524_288_000);
    assert!(matches!(config.accounts_db.block_size, BlockSize::Block512));
}
