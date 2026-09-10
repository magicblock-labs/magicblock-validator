use std::{ffi::OsString, fs::File, io::Write, path::PathBuf, time::Duration};

use serial_test::parallel;
use tempfile::TempDir;

use crate::{LeaderParams, consts};

fn run_cli(args: &[&str]) -> LeaderParams {
    let args = std::iter::once("validator")
        .chain(args.iter().copied())
        .map(OsString::from);
    LeaderParams::try_new(args).expect("configuration should parse")
}

fn create_temp_config(content: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("temporary directory should be created");
    let path = dir.path().join("magicblock.toml");
    let mut file =
        File::create(&path).expect("configuration should be created");
    writeln!(file, "{content}").expect("configuration should be written");
    (dir, path)
}

#[test]
#[parallel]
fn defaults_use_leader_engine_configuration() {
    let config = run_cli(&[]);

    assert_eq!(
        config.engine.ledger.directory,
        PathBuf::from(consts::DEFAULT_ENGINE_LEDGER_DIRECTORY),
    );
    assert_eq!(
        config.engine.accountsdb.directory,
        config.engine.ledger.directory.join("accountsdb"),
    );
    assert_eq!(
        config.engine.accountsdb.lru_capacity,
        consts::DEFAULT_ACCOUNTS_LRU_CAPACITY,
    );
    assert_eq!(
        config.engine.blockstore.blocktime,
        Duration::from_millis(consts::DEFAULT_LEDGER_BLOCK_TIME_MS),
    );
    assert_eq!(
        config.engine.blockstore.superblock.get(),
        consts::DEFAULT_SUPERBLOCK_SIZE,
    );
    assert_eq!(
        config.engine.replication.bind_address.0,
        consts::DEFAULT_REPLICATION_BIND_ADDRESS
            .parse()
            .expect("default replication address should parse"),
    );
    assert!(config.engine.replication.allowed_followers.is_empty());
    assert_eq!(config.remotes.len(), 2);
}

#[test]
#[parallel]
fn engine_configuration_loads_from_toml() {
    let (_dir, path) = create_temp_config(
        r#"
        [engine.accountsdb]
        directory = "/var/lib/magicblock/accounts"
        lru-capacity = 9000

        [engine.blockstore]
        blocktime = "250ms"
        superblock = 20

        [engine.ledger]
        directory = "/var/lib/magicblock/ledger"
        size-limit = 1073741824

        [engine.replication]
        bind-address = "0.0.0.0:11000"
        "#,
    );

    let config = run_cli(&[path.to_str().expect("UTF-8 path")]);

    assert_eq!(
        config.engine.accountsdb.directory,
        PathBuf::from("/var/lib/magicblock/accounts"),
    );
    assert_eq!(config.engine.accountsdb.lru_capacity, 9000);
    assert_eq!(
        config.engine.blockstore.blocktime,
        Duration::from_millis(250),
    );
    assert_eq!(config.engine.blockstore.superblock.get(), 20);
    assert_eq!(
        config.engine.ledger.directory,
        PathBuf::from("/var/lib/magicblock/ledger"),
    );
    assert_eq!(config.engine.ledger.size_limit, 1_073_741_824);
    assert_eq!(
        config.engine.replication.bind_address.0,
        "0.0.0.0:11000".parse().expect("address should parse"),
    );
}

#[test]
#[parallel]
fn risk_configuration_loads_from_toml() {
    let (_dir, path) = create_temp_config(
        r#"
        [chainlink.risk]
        enabled = true
        risk-server-url = "http://risk.example:3001"
        request-timeout = "2s"
        "#,
    );

    let config = run_cli(&[path.to_str().expect("UTF-8 path")]);

    assert!(config.chainlink.risk.enabled);
    assert_eq!(
        config.chainlink.risk.risk_server_url,
        "http://risk.example:3001",
    );
    assert_eq!(
        config.chainlink.risk.request_timeout,
        Duration::from_secs(2)
    );
}

#[test]
#[parallel]
fn follower_authority_is_rejected_by_parent_config() {
    let (_dir, path) = create_temp_config(
        r#"
        [engine.authority]
        local = "9Vo7TbA5YfC5a33JhAi9Fb41usA6JwecHNRw3f9MzzHAM8hFnXTzL5DcEHwsAFjuUZ8vNQcJ4XziRFpMc3gTgBQ"
        remote = "11111111111111111111111111111111"
        "#,
    );
    let args = std::iter::once("validator")
        .chain([path.to_str().expect("UTF-8 path")])
        .map(OsString::from);

    let error = LeaderParams::try_new(args)
        .expect_err("leader parent config should reject remote authority");

    assert!(
        error
            .to_string()
            .contains("reserved for follower validators"),
        "unexpected error: {error}",
    );
}

#[test]
#[parallel]
fn display_is_compact_and_redacts_sensitive_configuration() {
    let config = run_cli(&[]);
    let local_keypair = config.engine.authority.local.to_base58_string();
    let rendered = config.to_string();

    assert!(rendered.contains("│ Setting"));
    assert!(rendered.contains("│ Role"));
    assert!(rendered.contains("leader"));
    assert!(rendered.contains("AccountsDB"));
    assert!(!rendered.contains(&local_keypair));
    assert!(!rendered.contains(consts::DEFAULT_REMOTE));
    assert!(!rendered.contains("[engine"));
}

#[test]
#[parallel]
fn invalid_aperture_port_is_rejected() {
    let (_dir, path) = create_temp_config(
        r#"
        [aperture]
        listen = "127.0.0.1:65535"
        "#,
    );
    let args = std::iter::once("validator")
        .chain([path.to_str().expect("UTF-8 path")])
        .map(OsString::from);

    let error = LeaderParams::try_new(args)
        .expect_err("port without a websocket successor should fail");

    assert!(error.to_string().contains("port 65535 is invalid"));
}

#[test]
#[parallel]
fn example_configuration_parses() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../config.example.toml");

    let config = run_cli(&[path.to_str().expect("UTF-8 path")]);

    assert!(!config.chainlink.risk.enabled);
    assert_eq!(
        config.chainlink.risk.risk_server_url,
        consts::DEFAULT_RISK_SERVER_URL,
    );
    assert_eq!(
        config.chainlink.risk.request_timeout,
        Duration::from_secs(consts::DEFAULT_RISK_REQUEST_TIMEOUT_SEC),
    );
}
