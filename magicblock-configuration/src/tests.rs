use crate::{
    config::{BlockSize, LifecycleMode},
    consts, MagicBlockParams, RemoteCluster,
};
use serial_test::{parallel, serial};
use solana_keypair::Keypair;
use std::{ffi::OsString, fs::File, io::Write, path::PathBuf, str::FromStr};
use tempfile::TempDir;

// ============================================================================
// 1. Test Infrastructure & Helpers
// ============================================================================

/// Simulates running the binary with the provided CLI arguments.
/// Automatically prepends the binary name.
fn run_cli(args: Vec<&str>) -> MagicBlockParams {
    let itr = std::iter::once("validator")
        .chain(args.into_iter())
        .map(OsString::from);

    MagicBlockParams::try_new(itr).expect("Failed to parse configuration")
}

/// Creates a temporary TOML file with the given content.
/// Returns the TempDir (to prevent early cleanup) and the file path.
fn create_temp_config(content: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("magicblock.toml");
    let mut file = File::create(&file_path).unwrap();
    writeln!(&mut file, "{}", content).unwrap();
    (dir, file_path)
}

/// RAII Guard to safely set/unset Environment Variables during tests.
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
// 2. Foundation Tests (Defaults & Serialization)
// ============================================================================

#[test]
#[parallel]
fn test_defaults_are_sane() {
    let config = run_cli(vec![]);

    // Verify key defaults used in production
    assert_eq!(config.validator.basefee, consts::DEFAULT_BASE_FEE);
    assert_eq!(
        config.remote,
        RemoteCluster::from_str(consts::DEFAULT_REMOTE).unwrap()
    );
    assert_eq!(config.listen.0.port(), 8899);
    assert_eq!(config.lifecycle, LifecycleMode::Ephemeral);

    // Verify internal config defaults (not exposed to CLI)
    assert_eq!(config.accountsdb.database_size, 100 * 1024 * 1024); // 100MB
    assert!(matches!(config.accountsdb.block_size, BlockSize::Block256));
}

#[test]
#[parallel]
fn test_load_basic_toml() {
    let (_dir, config_path) = create_temp_config(
        r#"
        lifecycle = "offline"
        storage = "/var/lib/magicblock"
        "#,
    );

    let config = run_cli(vec!["--config", config_path.to_str().unwrap()]);

    assert_eq!(config.lifecycle, LifecycleMode::Offline);
    assert_eq!(
        config.storage.unwrap().to_str().unwrap(),
        "/var/lib/magicblock"
    );
}

// ============================================================================
// 3. Precedence Tests: CLI > Env > TOML > Default
// ============================================================================

#[test]
#[serial]
fn test_env_overrides_toml() {
    // TOML says Replica, Env says Offline. Env should win.
    let (_dir, config_path) = create_temp_config(r#"lifecycle = "replica""#);
    let _env = EnvVarGuard::new("MBV_LIFECYCLE", "offline");

    let config = run_cli(vec!["--config", config_path.to_str().unwrap()]);

    assert_eq!(config.lifecycle, LifecycleMode::Offline);
}

#[test]
#[parallel]
fn test_cli_overrides_toml() {
    // TOML says 100, CLI says 500. CLI should win.
    let (_dir, config_path) = create_temp_config(
        r#"
        [validator]
        basefee = 100
        "#,
    );

    let config = run_cli(vec![
        "--config",
        config_path.to_str().unwrap(),
        "--basefee",
        "500",
    ]);

    assert_eq!(config.validator.basefee, 500);
}

#[test]
#[serial]
fn test_cli_overrides_env() {
    // Env says 1000, CLI says 2000. CLI should win.
    let _env = EnvVarGuard::new("MBV_VALIDATOR_BASEFEE", "1000");

    let config = run_cli(vec!["--basefee", "2000"]);

    assert_eq!(config.validator.basefee, 2000);
}

#[test]
#[serial]
fn test_full_stack_precedence() {
    // TOML=100, ENV=200, CLI=300. Result must be 300.
    let (_dir, config_path) = create_temp_config(
        r#"
        [validator]
        basefee = 100
        "#,
    );
    let _env = EnvVarGuard::new("MBV_VALIDATOR_BASEFEE", "200");

    let config = run_cli(vec![
        "--config",
        config_path.to_str().unwrap(),
        "--basefee",
        "300",
    ]);

    assert_eq!(config.validator.basefee, 300);
}

// ============================================================================
// 4. The "Overlay" Logic (Non-Destructive Updates)
// ============================================================================

#[test]
#[parallel]
fn test_cli_overlay_is_non_destructive() {
    // CRITICAL: Ensure providing ONE CLI arg (basefee) does NOT reset
    // other fields in the same struct (keypair) back to defaults.

    let custom_keypair = Keypair::new().to_base58_string();
    let (_dir, config_path) = create_temp_config(&format!(
        r#"
        [validator]
        basefee = 100
        keypair = "{}"
        "#,
        custom_keypair
    ));

    // Change ONLY basefee via CLI
    let config = run_cli(vec![
        "--config",
        config_path.to_str().unwrap(),
        "--basefee",
        "500",
    ]);

    // Basefee updated
    assert_eq!(config.validator.basefee, 500);
    // Keypair PRESERVED from TOML
    assert_eq!(config.validator.keypair.to_string(), custom_keypair);
}

#[test]
#[parallel]
fn test_cli_does_not_touch_file_only_fields() {
    // Ensure CLI parsing doesn't accidentally wipe fields that aren't in CliParams,
    // like `accountsdb` or `programs`.
    let (_dir, config_path) = create_temp_config(
        r#"
        [accountsdb]
        database-size = 999
        "#,
    );

    let config = run_cli(vec![
        "--config",
        config_path.to_str().unwrap(),
        "--basefee",
        "500",
    ]);

    // File-only setting preserved
    assert_eq!(config.accountsdb.database_size, 999);
    // CLI setting applied
    assert_eq!(config.validator.basefee, 500);
}

// ============================================================================
// 5. Deep & Complex Structures
// ============================================================================

#[test]
#[serial]
fn test_env_vars_deep_mapping() {
    // Verify we can set deeply nested fields via Env vars using underscores
    // MBV_ACCOUNTS_DB_DATABASE_SIZE -> accounts_db.database_size
    let _guard = EnvVarGuard::new("MBV_ACCOUNTSDB_DATABASE-SIZE", "4096");

    let config = run_cli(vec![]);
    assert_eq!(config.accountsdb.database_size, 4096);
}

#[test]
#[parallel]
fn test_loading_programs_list() {
    // The `programs` field is a Vec which can only be set via TOML.
    // This tests Serde handling of TOML arrays of tables.
    let (_dir, config_path) = create_temp_config(
        r#"
        [[programs]]
        id = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        path = "/tmp/token.so"

        [[programs]]
        id = "Memo1UhkJRfHyvLMcVucJwxXeuD728EqVDDwQDxFMNo"
        path = "/tmp/memo.so"
        "#,
    );

    let config = run_cli(vec!["--config", config_path.to_str().unwrap()]);

    assert_eq!(config.programs.len(), 2);
    assert_eq!(
        config.programs[0].id.to_string(),
        "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
    );
    assert_eq!(config.programs[1].path.to_str().unwrap(), "/tmp/memo.so");
}

#[test]
#[parallel]
fn test_chainlink_config() {
    // Verify sub-structs not exposed to CLI load correctly from file
    let (_dir, config_path) = create_temp_config(
        r#"
        [chainlink]
        prepare-lookup-tables = true
        max-monitored-accounts = 5000
        "#,
    );

    let config = run_cli(vec!["--config", config_path.to_str().unwrap()]);

    assert_eq!(config.chainlink.prepare_lookup_tables, true);
    assert_eq!(config.chainlink.max_monitored_accounts, 5000);
}

// ============================================================================
// 6. Type Parsing & Validation
// ============================================================================

#[test]
#[parallel]
fn test_parse_remote_variants() {
    // 1. Alias expansion
    let c1 = run_cli(vec!["--remote", "mainnet"]);
    match c1.remote {
        RemoteCluster::Single(crate::types::Remote::Unified(u)) => {
            assert_eq!(u.0.as_str(), consts::MAINNET_URL);
        }
        _ => panic!("Failed to parse 'mainnet' alias"),
    }

    // 2. Explicit URL
    let custom = "http://127.0.0.1:3000/";
    let c2 = run_cli(vec!["--remote", custom]);
    match c2.remote {
        RemoteCluster::Single(crate::types::Remote::Unified(u)) => {
            assert_eq!(u.0.as_str(), custom);
        }
        _ => panic!("Failed to parse custom URL"),
    }
}

#[test]
#[parallel]
fn test_parse_enum_case_insensitivity() {
    // TOML handles enums as strings
    let (_dir, config_path) = create_temp_config(
        r#"
        [accountsdb]
        block-size = "block512"
        "#,
    );

    let config = run_cli(vec!["--config", config_path.to_str().unwrap()]);
    assert!(matches!(config.accountsdb.block_size, BlockSize::Block512));
}
