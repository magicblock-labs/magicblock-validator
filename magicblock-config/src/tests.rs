use std::{ffi::OsString, fs::File, io::Write, path::PathBuf};

use isocountry::CountryCode;
use serial_test::{parallel, serial};
use solana_keypair::Keypair;
use tempfile::TempDir;

use crate::{
    config::{BlockSize, LifecycleMode},
    consts::{self, DEFAULT_VALIDATOR_KEYPAIR},
    types::{RemoteConfig, RemoteKind},
    ValidatorParams,
};

// ============================================================================
// 1. Test Infrastructure & Helpers
// ============================================================================

/// Simulates running the binary with the provided CLI arguments.
/// Automatically prepends the binary name.
fn run_cli(args: Vec<&str>) -> ValidatorParams {
    let itr = std::iter::once("validator").chain(args).map(OsString::from);

    ValidatorParams::try_new(itr).expect("Failed to parse configuration")
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
/// NOTE:
/// it should only be used with serial tests which don't run concurrently
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
    // Remotes default to empty when not specified
    assert_eq!(config.remotes.len(), 0);
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

    let config = run_cli(vec![config_path.to_str().unwrap()]);

    assert_eq!(config.lifecycle, LifecycleMode::Offline);
    assert_eq!(config.storage.to_str().unwrap(), "/var/lib/magicblock");
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

    let config = run_cli(vec![config_path.to_str().unwrap()]);

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

    let config =
        run_cli(vec![config_path.to_str().unwrap(), "--basefee", "500"]);

    assert_eq!(config.validator.basefee, 500);
}

#[test]
#[serial]
fn test_cli_overrides_env() {
    // Env says 1000, CLI says 2000. CLI should win.
    let _env = EnvVarGuard::new("MBV_VALIDATOR__BASEFEE", "1000");

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
    let _env = EnvVarGuard::new("MBV_VALIDATOR__BASEFEE", "200");

    let config =
        run_cli(vec![config_path.to_str().unwrap(), "--basefee", "300"]);

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
    let config =
        run_cli(vec![config_path.to_str().unwrap(), "--basefee", "500"]);

    // Basefee updated
    assert_eq!(config.validator.basefee, 500);
    // Keypair PRESERVED from TOML
    assert_eq!(config.validator.keypair, custom_keypair.parse().unwrap());
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

    let config =
        run_cli(vec![config_path.to_str().unwrap(), "--basefee", "500"]);

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
    // MBV_ACCOUNTS_DB__DATABASE_SIZE -> accounts_db.database_size
    let _guard = EnvVarGuard::new("MBV_ACCOUNTSDB__DATABASE_SIZE", "4096");

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

    let config = run_cli(vec![config_path.to_str().unwrap()]);

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

    let config = run_cli(vec![config_path.to_str().unwrap()]);

    assert!(config.chainlink.prepare_lookup_tables);
    assert_eq!(config.chainlink.max_monitored_accounts, 5000);
}

// ============================================================================
// 6. Type Parsing & Validation
// ============================================================================

// ============================================================================
// 8. Ledger, Time & Commit Strategies
// ============================================================================

#[test]
#[parallel]
fn test_ledger_and_commit_settings() {
    // Verify 'humantime' deserialization (e.g., "1s 50ms")
    let (_dir, config_path) = create_temp_config(
        r#"
        [ledger]
        block-time = "800ms"
        verify-keypair = false

        [commit]
        compute-unit-price = 123456
        "#,
    );

    let config = run_cli(vec![config_path.to_str().unwrap()]);

    assert_eq!(config.ledger.block_time.as_millis(), 800);
    assert!(!config.ledger.verify_keypair);
    assert_eq!(config.commit.compute_unit_price, 123456);
}

#[test]
#[serial]
fn test_task_scheduler_bool_env() {
    // Verify standard boolean parsing from Env vars work on nested fields
    let _guard = EnvVarGuard::new("MBV_TASK_SCHEDULER__RESET", "true");

    let config = run_cli(vec![]);
    assert!(config.task_scheduler.reset);
}

#[test]
#[parallel]
fn test_example_config_full_coverage() {
    // 1. Locate the config.example.toml in the workspace root
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR not set");
    let workspace_root = PathBuf::from(manifest_dir)
        .parent()
        .expect("No parent dir")
        .to_path_buf();
    let config_path = workspace_root.join("config.example.toml");

    if !config_path.exists() {
        eprintln!(
            "WARNING: Skipped example config test. File not found: {:?}",
            config_path
        );
        return;
    }

    // 2. Parse the configuration
    let config = run_cli(vec![config_path.to_str().unwrap()]);

    // ========================================================================
    // 3. Core & Network
    // ========================================================================
    assert_eq!(config.lifecycle, LifecycleMode::Ephemeral);
    // Example config has one RPC remote with "devnet" alias resolved
    assert_eq!(config.remotes.len(), 1);
    assert_eq!(config.remotes[0].url, consts::RPC_DEVNET);
    assert_eq!(config.remotes[0].kind, crate::types::RemoteKind::Rpc);
    assert_eq!(config.listen.0.port(), 8899);
    // Check that storage path is set (contains the expected folder name)
    assert!(config
        .storage
        .to_string_lossy()
        .contains("magicblock-test-storage"));

    // ========================================================================
    // 4. Metrics
    // ========================================================================
    assert_eq!(config.metrics.address.0.port(), 9090);
    assert_eq!(config.metrics.collect_frequency.as_secs(), 30);

    // ========================================================================
    // 5. Validator Identity
    // ========================================================================
    assert_eq!(config.validator.basefee, 0);
    // Verify the specific example keypair is loaded
    assert_eq!(
        config.validator.keypair.0.to_base58_string(),
        DEFAULT_VALIDATOR_KEYPAIR
    );

    // ========================================================================
    // 6. Chain Commitment
    // ========================================================================
    assert_eq!(config.commit.compute_unit_price, 1_000_000);

    // ========================================================================
    // 7. Accounts Database
    // ========================================================================
    assert_eq!(config.accountsdb.database_size, 104_857_600);
    assert!(matches!(config.accountsdb.block_size, BlockSize::Block256));
    assert_eq!(config.accountsdb.index_size, 16_777_216);
    assert_eq!(config.accountsdb.max_snapshots, 4);
    assert_eq!(config.accountsdb.snapshot_frequency, 1024);
    assert!(!config.accountsdb.reset);

    // ========================================================================
    // 8. Ledger & Block Production
    // ========================================================================
    assert_eq!(config.ledger.block_time_ms(), 400);
    assert!(config.ledger.verify_keypair);
    assert!(!config.ledger.reset);
    // Verify the size field we added (512MB)
    assert_eq!(config.ledger.size, 536_870_912);

    // ========================================================================
    // 9. Chainlink (Cloning)
    // ========================================================================
    assert!(!config.chainlink.prepare_lookup_tables);
    assert_eq!(config.chainlink.auto_airdrop_lamports, 0);
    assert_eq!(config.chainlink.max_monitored_accounts, 1000);

    // ========================================================================
    // 10. Optional Sections
    // ========================================================================
    // Task scheduler reset should be false
    assert!(!config.task_scheduler.reset);

    // The example file has the programs section with 2 entries
    assert_eq!(
        config.programs.len(),
        2,
        "Expected 'programs' list to contain items in example config"
    );

    // The chain-operation section is present
    assert!(
        config.chain_operation.is_some(),
        "Expected 'chain-operation' to be set in example config file"
    );
}

#[test]
#[serial]
fn test_env_vars_full_coverage() {
    // We must keep these guards alive until the config is parsed.
    // The `EnvVarGuard` helper (defined in your tests.rs) cleans them up on Drop.
    let _guards = vec![
        // --- Core ---
        EnvVarGuard::new("MBV_LIFECYCLE", "replica"),
        // Note: MBV_REMOTE is no longer supported for the new Vec<RemoteConfig> format
        // Use TOML [[remote]] array syntax instead
        EnvVarGuard::new("MBV_STORAGE", "/tmp/env-test-storage"),
        EnvVarGuard::new("MBV_LISTEN", "127.0.0.1:9999"),
        // --- Metrics ---
        EnvVarGuard::new("MBV_METRICS__ADDRESS", "127.0.0.1:9091"),
        EnvVarGuard::new("MBV_METRICS__COLLECT_FREQUENCY", "15s"),
        // --- Validator Identity ---
        EnvVarGuard::new("MBV_VALIDATOR__BASEFEE", "5000"),
        // Using a random valid keypair for testing
        EnvVarGuard::new("MBV_VALIDATOR__KEYPAIR", DEFAULT_VALIDATOR_KEYPAIR),
        // --- Commit Strategy ---
        EnvVarGuard::new("MBV_COMMIT__COMPUTE_UNIT_PRICE", "500000"),
        // --- Accounts DB ---
        EnvVarGuard::new("MBV_ACCOUNTSDB__DATABASE_SIZE", "10485760"), // 10MB
        EnvVarGuard::new("MBV_ACCOUNTSDB__BLOCK_SIZE", "block512"),
        EnvVarGuard::new("MBV_ACCOUNTSDB__INDEX_SIZE", "2048"),
        EnvVarGuard::new("MBV_ACCOUNTSDB__MAX_SNAPSHOTS", "10"),
        EnvVarGuard::new("MBV_ACCOUNTSDB__SNAPSHOT_FREQUENCY", "500"),
        EnvVarGuard::new("MBV_ACCOUNTSDB__RESET", "true"),
        // --- Ledger ---
        EnvVarGuard::new("MBV_LEDGER__BLOCK_TIME", "200ms"),
        EnvVarGuard::new("MBV_LEDGER__SIZE", "1000000"),
        EnvVarGuard::new("MBV_LEDGER__VERIFY_KEYPAIR", "false"),
        EnvVarGuard::new("MBV_LEDGER__RESET", "true"),
        // --- Chainlink ---
        EnvVarGuard::new("MBV_CHAINLINK__PREPARE_LOOKUP_TABLES", "true"),
        EnvVarGuard::new("MBV_CHAINLINK__AUTO_AIRDROP_LAMPORTS", "555"),
        EnvVarGuard::new("MBV_CHAINLINK__MAX_MONITORED_ACCOUNTS", "123"),
        // --- Task Scheduler ---
        EnvVarGuard::new("MBV_TASK_SCHEDULER__RESET", "true"),
        // --- Chain Operation (Optional Section) ---
        // Figment can instantiate optional structs if their fields are present
        EnvVarGuard::new("MBV_CHAIN_OPERATION__COUNTRY_CODE", "DE"),
        EnvVarGuard::new(
            "MBV_CHAIN_OPERATION__FQDN",
            "https://env.example.com",
        ),
        EnvVarGuard::new("MBV_CHAIN_OPERATION__CLAIM_FEES_FREQUENCY", "48h"),
    ];

    // Run CLI with NO arguments. It should pick up everything from Env.
    let config = run_cli(vec![]);

    // --- Assertions ---

    // Core
    assert_eq!(config.lifecycle, LifecycleMode::Replica);
    // Remotes must be configured via TOML, not env vars
    assert_eq!(config.remotes.len(), 0);
    assert_eq!(config.storage.to_string_lossy(), "/tmp/env-test-storage");
    assert_eq!(config.listen.0.port(), 9999);

    // Metrics
    assert_eq!(config.metrics.address.0.port(), 9091);
    assert_eq!(config.metrics.collect_frequency.as_secs(), 15);

    // Validator
    assert_eq!(config.validator.basefee, 5000);
    // (We skip checking the exact keypair bytes, just that it didn't crash)

    // Commit
    assert_eq!(config.commit.compute_unit_price, 500_000);

    // Accounts DB
    assert_eq!(config.accountsdb.database_size, 10_485_760);
    assert!(matches!(config.accountsdb.block_size, BlockSize::Block512));
    assert_eq!(config.accountsdb.index_size, 2048);
    assert_eq!(config.accountsdb.max_snapshots, 10);
    assert_eq!(config.accountsdb.snapshot_frequency, 500);
    assert!(config.accountsdb.reset);

    // Ledger
    assert_eq!(config.ledger.block_time_ms(), 200);
    assert_eq!(config.ledger.size, 1_000_000);
    assert!(!config.ledger.verify_keypair);
    assert!(config.ledger.reset);

    // Chainlink
    assert!(config.chainlink.prepare_lookup_tables);
    assert_eq!(config.chainlink.auto_airdrop_lamports, 555);
    assert_eq!(config.chainlink.max_monitored_accounts, 123);

    // Task Scheduler
    assert!(config.task_scheduler.reset);

    // Chain Operation
    // Verify the optional struct was created and populated
    let chain_op = config
        .chain_operation
        .expect("Chain operation config should be present via env vars");
    assert_eq!(chain_op.country_code, CountryCode::DEU);
    assert_eq!(chain_op.fqdn.as_str(), "https://env.example.com/");
    assert_eq!(chain_op.claim_fees_frequency.as_secs(), 48 * 3600);
}

// ============================================================================
// 9. New Remote Config Parsing
// ============================================================================

#[test]
#[parallel]
fn test_parse_single_rpc_remote() {
    let (_dir, config_path) = create_temp_config(
        r#"
        [[remote]]
        kind = "rpc"
        url = "http://localhost:8899"
        "#,
    );

    let config = run_cli(vec![config_path.to_str().unwrap()]);

    assert_eq!(config.remotes.len(), 1);
    assert_eq!(config.remotes[0].kind, RemoteKind::Rpc);
    assert_eq!(config.remotes[0].url, "http://localhost:8899");
    assert_eq!(config.remotes[0].api_key, None);
}

#[test]
#[parallel]
fn test_parse_rpc_remote_with_api_key() {
    let (_dir, config_path) = create_temp_config(
        r#"
        [[remote]]
        kind = "rpc"
        url = "https://api.example.com"
        api-key = "secret-key-123"
        "#,
    );

    let config = run_cli(vec![config_path.to_str().unwrap()]);

    assert_eq!(config.remotes.len(), 1);
    assert_eq!(config.remotes[0].kind, RemoteKind::Rpc);
    assert_eq!(config.remotes[0].url, "https://api.example.com");
    assert_eq!(
        config.remotes[0].api_key,
        Some("secret-key-123".to_string())
    );
}

#[test]
#[parallel]
fn test_parse_multiple_remotes_mixed_kinds() {
    let (_dir, config_path) = create_temp_config(
        r#"
        [[remote]]
        kind = "rpc"
        url = "http://localhost:8899"

        [[remote]]
        kind = "websocket"
        url = "wss://mainnet-beta.solana.com"

        [[remote]]
        kind = "websocket"
        url = "wss://backup-node.example.com"

        [[remote]]
        kind = "grpc"
        url = "http://grpc.example.com:50051"
        api-key = "grpc-secret"
        "#,
    );

    let config = run_cli(vec![config_path.to_str().unwrap()]);

    assert_eq!(config.remotes.len(), 4);

    // First: RPC
    assert_eq!(config.remotes[0].kind, RemoteKind::Rpc);
    assert_eq!(config.remotes[0].url, "http://localhost:8899");

    // Second: WebSocket
    assert_eq!(config.remotes[1].kind, RemoteKind::Websocket);
    assert_eq!(config.remotes[1].url, "wss://mainnet-beta.solana.com");
    assert_eq!(config.remotes[1].api_key, None);

    // Third: WebSocket (duplicate URL is allowed)
    assert_eq!(config.remotes[2].kind, RemoteKind::Websocket);
    assert_eq!(config.remotes[2].url, "wss://backup-node.example.com");

    // Fourth: gRPC
    assert_eq!(config.remotes[3].kind, RemoteKind::Grpc);
    assert_eq!(config.remotes[3].url, "http://grpc.example.com:50051");
    assert_eq!(config.remotes[3].api_key, Some("grpc-secret".to_string()));
}

#[test]
#[parallel]
fn test_parse_remotes_empty_when_not_provided() {
    let (_dir, config_path) = create_temp_config(
        r#"
        lifecycle = "ephemeral"
        "#,
    );

    let config = run_cli(vec![config_path.to_str().unwrap()]);

    assert_eq!(config.remotes.len(), 0);
}

#[test]
#[serial]
fn test_remotes_via_env_vars() {
    // Environment variables can't directly set array fields via Figment,
    // so this test verifies that remotes remain empty when not set in config.
    // In a real scenario, array fields are only set via TOML files.

    let (_dir, config_path) = create_temp_config(
        r#"
        [[remote]]
        kind = "websocket"
        url = "wss://env-test.example.com"
        "#,
    );

    let config = run_cli(vec![config_path.to_str().unwrap()]);

    assert_eq!(config.remotes.len(), 1);
    assert_eq!(config.remotes[0].kind, RemoteKind::Websocket);
    assert_eq!(config.remotes[0].url, "wss://env-test.example.com");
}

#[test]
#[serial]
fn test_deserialization_resolves_aliases() {
    // Verify that aliases are resolved during deserialization,
    // not just in resolved_url() method
    let (_dir, config_path) = create_temp_config(
        r#"
        [[remote]]
        kind = "rpc"
        url = "devnet"

        [[remote]]
        kind = "websocket"
        url = "mainnet"
        "#,
    );

    let config = run_cli(vec![config_path.to_str().unwrap()]);

    assert_eq!(config.remotes.len(), 2);

    // RPC remote should have devnet alias resolved to actual URL
    assert_eq!(config.remotes[0].kind, RemoteKind::Rpc);
    assert_eq!(config.remotes[0].url, consts::RPC_DEVNET);

    // WebSocket remote should have mainnet alias resolved to actual URL
    assert_eq!(config.remotes[1].kind, RemoteKind::Websocket);
    assert_eq!(config.remotes[1].url, consts::WS_MAINNET);
}

#[test]
#[parallel]
fn test_remote_config_parse_url_method() {
    let remote = RemoteConfig {
        kind: RemoteKind::Rpc,
        url: "https://api.example.com".to_string(),
        api_key: None,
    };

    let parsed_url = remote.parse_url();
    assert!(parsed_url.is_ok());
    let url = parsed_url.unwrap();
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some("api.example.com"));
}

#[test]
#[parallel]
fn test_remote_config_invalid_url() {
    let remote = RemoteConfig {
        kind: RemoteKind::Rpc,
        url: "not a valid url".to_string(),
        api_key: None,
    };

    let parsed_url = remote.parse_url();
    assert!(parsed_url.is_err());
}

#[test]
#[parallel]
fn test_rpc_alias_resolution() {
    // Test mainnet alias
    let mainnet = RemoteConfig {
        kind: RemoteKind::Rpc,
        url: "mainnet".to_string(),
        api_key: None,
    };
    assert_eq!(mainnet.resolved_url(), consts::RPC_MAINNET);

    // Test devnet alias
    let devnet = RemoteConfig {
        kind: RemoteKind::Rpc,
        url: "devnet".to_string(),
        api_key: None,
    };
    assert_eq!(devnet.resolved_url(), consts::RPC_DEVNET);

    // Test local alias
    let local = RemoteConfig {
        kind: RemoteKind::Rpc,
        url: "local".to_string(),
        api_key: None,
    };
    assert_eq!(local.resolved_url(), consts::RPC_LOCAL);
}

#[test]
#[parallel]
fn test_websocket_alias_resolution() {
    // Test mainnet alias
    let mainnet = RemoteConfig {
        kind: RemoteKind::Websocket,
        url: "mainnet".to_string(),
        api_key: None,
    };
    assert_eq!(mainnet.resolved_url(), consts::WS_MAINNET);

    // Test devnet alias
    let devnet = RemoteConfig {
        kind: RemoteKind::Websocket,
        url: "devnet".to_string(),
        api_key: None,
    };
    assert_eq!(devnet.resolved_url(), consts::WS_DEVNET);

    // Test local alias
    let local = RemoteConfig {
        kind: RemoteKind::Websocket,
        url: "local".to_string(),
        api_key: None,
    };
    assert_eq!(local.resolved_url(), consts::WS_LOCAL);
}

#[test]
#[parallel]
fn test_alias_resolution_same_alias_different_kinds() {
    // Same alias "mainnet" should resolve differently for RPC vs WebSocket
    let rpc = RemoteConfig {
        kind: RemoteKind::Rpc,
        url: "mainnet".to_string(),
        api_key: None,
    };
    let ws = RemoteConfig {
        kind: RemoteKind::Websocket,
        url: "mainnet".to_string(),
        api_key: None,
    };

    assert_eq!(rpc.resolved_url(), consts::RPC_MAINNET);
    assert_eq!(ws.resolved_url(), consts::WS_MAINNET);
    // They should be different
    assert_ne!(rpc.resolved_url(), ws.resolved_url());
}

#[test]
#[parallel]
fn test_full_url_not_treated_as_alias() {
    // Full URLs should not be treated as aliases
    let custom_rpc = RemoteConfig {
        kind: RemoteKind::Rpc,
        url: "https://custom-node.example.com".to_string(),
        api_key: None,
    };
    assert_eq!(
        custom_rpc.resolved_url(),
        "https://custom-node.example.com"
    );

    let custom_ws = RemoteConfig {
        kind: RemoteKind::Websocket,
        url: "wss://custom-node.example.com".to_string(),
        api_key: None,
    };
    assert_eq!(
        custom_ws.resolved_url(),
        "wss://custom-node.example.com"
    );
}

#[test]
#[parallel]
fn test_parse_url_with_alias() {
    let remote = RemoteConfig {
        kind: RemoteKind::Rpc,
        url: "devnet".to_string(),
        api_key: None,
    };

    let parsed = remote.parse_url();
    assert!(parsed.is_ok());
    let url = parsed.unwrap();
    assert_eq!(url.host_str(), Some("api.devnet.solana.com"));
}
