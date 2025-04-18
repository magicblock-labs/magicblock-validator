use std::{net::IpAddr, str::FromStr};

use clap::{Parser, ValueEnum};
use isocountry::CountryCode;
use magicblock_accounts_db::config::{AccountsDbConfig, BlockSize};
use magicblock_api::EphemeralConfig;
use magicblock_config::{
    AllowedProgram, CommitStrategy, GeyserGrpcConfig, LedgerConfig,
    LifecycleMode, Payer, PayerParams, ProgramConfig, RemoteConfig, RpcConfig,
    ValidatorConfig,
};
use solana_sdk::pubkey::Pubkey;
use url::Url;

/// MagicBlock Validator CLI arguments
#[derive(Parser, Debug)]
#[command(name = "MagicBlock Validator")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Runs a MagicBlock validator node")]
pub struct Cli {
    /// Path to the configuration file
    pub config_path: Option<String>,

    /// Base58 encoded validator private key
    #[arg(
        short = 'k',
        long,
        value_name = "KEYPAIR",
        env = "VALIDATOR_KEYPAIR",
        help = "Base58 encoded private key for the validator."
    )]
    pub keypair: Option<String>,

    /// Disable geyser components (accounts,transactions)
    #[arg(
        long,
        value_name = "COMPONENTS",
        env = "GEYSER_DISABLE",
        help = "Specifies geyser components to disable. [default: (accounts,transactions)]"
    )]
    pub disable_geyser: Option<String>,

    /// Disable geyser cache components (accounts,transactions)
    #[arg(
        long,
        value_name = "COMPONENTS",
        env = "GEYSER_CACHE_DISABLE",
        help = "Specifies geyser cache components to disable. [default: (accounts,transactions)]"
    )]
    pub disable_geyser_cache: Option<String>,

    #[command(flatten)]
    pub config: ConfigArgs,
}

#[derive(Parser, Debug)]
pub struct ConfigArgs {
    #[command(flatten)]
    accounts: AccountsArgs,
    #[command(flatten)]
    rpc: RpcArgs,
    #[command(flatten)]
    geyser_grpc: GeyserGrpcArgs,
    #[command(flatten)]
    validator: ValidatorConfigArgs,
    #[command(flatten)]
    ledger: LedgerConfigArgs,
    /// List of programs to add on startup. Format: <program_id>:<program_path>
    #[arg(long, value_parser = program_config_parser)]
    programs: Vec<ProgramConfig>,
    #[command(flatten)]
    metrics: MetricsConfig,
}

impl ConfigArgs {
    pub fn override_config(
        &self,
        config: EphemeralConfig,
    ) -> Result<EphemeralConfig, String> {
        let mut config = config.clone();

        if let Some(remote) = self.accounts.remote {
            config.accounts.remote = match remote {
                RemoteConfigArg::Devnet => RemoteConfig::Devnet,
                RemoteConfigArg::Mainnet => RemoteConfig::Mainnet,
                RemoteConfigArg::Testnet => RemoteConfig::Testnet,
                RemoteConfigArg::Development => RemoteConfig::Development,
            };
        } else if let Some(remote_custom) = &self.accounts.remote_custom {
            let url = Url::from_str(&remote_custom).map_err(|e| {
                format!("Failed to parse URL {}: {}", remote_custom, e)
            })?;

            if let Some(remote_custom_with_ws) =
                &self.accounts.remote_custom_with_ws
            {
                let url_ws =
                    Url::from_str(&remote_custom_with_ws).map_err(|e| {
                        format!(
                            "Failed to parse URL {}: {}",
                            remote_custom_with_ws, e
                        )
                    })?;
                config.accounts.remote =
                    RemoteConfig::CustomWithWs(url, url_ws);
            } else {
                config.accounts.remote = RemoteConfig::Custom(url);
            }
        }

        config.accounts.lifecycle = self.accounts.lifecycle.clone().into();
        config.accounts.commit = self.accounts.commit.clone().into();
        config.accounts.payer = self.accounts.payer.clone().into();
        config.accounts.allowed_programs = self
            .accounts
            .allowed_programs
            .clone()
            .into_iter()
            .map(|id| AllowedProgram {
                id: Pubkey::from_str_const(&id),
            })
            .collect();
        config.accounts.db = self.accounts.db.into();

        config.rpc = self.rpc.into();
        config.geyser_grpc = self.geyser_grpc.clone().into();
        config.validator = self.validator.clone().into();
        config.ledger = self.ledger.clone().into();
        config.programs = self.programs.clone();
        Ok(config)
    }
}

#[derive(Parser, Debug)]
struct AccountsArgs {
    /// Use a predefined remote configuration
    #[arg(
        long = "accounts-remote",
        conflicts_with = "remote_custom",
        conflicts_with = "remote_custom_with_ws"
    )]
    remote: Option<RemoteConfigArg>,
    /// Use a custom remote URL
    #[arg(
        long = "accounts-remote-custom",
        env = "ACCOUNTS_REMOTE",
        conflicts_with = "remote"
    )]
    remote_custom: Option<String>,
    /// Use a custom remote URL with WebSocket URL
    #[arg(
        long = "accounts-remote-custom-with-ws",
        env = "ACCOUNTS_REMOTE_WS",
        conflicts_with = "remote",
        requires = "remote_custom"
    )]
    remote_custom_with_ws: Option<String>,
    #[arg(
        long = "accounts-lifecycle",
        default_value = "programs-replica",
        env = "ACCOUNTS_LIFECYCLE"
    )]
    lifecycle: LifecycleModeArg,
    #[command(flatten)]
    commit: CommitStrategyArg,
    #[command(flatten)]
    payer: PayerArgs,
    /// List of allowed programs.
    #[arg(long = "accounts-allowed-programs")]
    allowed_programs: Vec<String>,
    #[command(flatten)]
    db: AccountsDbArgs,
}

#[derive(Parser, Debug, Default, Clone, Copy, ValueEnum)]
pub enum RemoteConfigArg {
    #[default]
    Devnet,
    Mainnet,
    Testnet,
    Development,
}

#[derive(Parser, Debug, Default, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum LifecycleModeArg {
    Replica,
    #[default]
    ProgramsReplica,
    Ephemeral,
    Offline,
}

impl Into<LifecycleMode> for LifecycleModeArg {
    fn into(self) -> LifecycleMode {
        match self {
            LifecycleModeArg::Replica => LifecycleMode::Replica,
            LifecycleModeArg::ProgramsReplica => LifecycleMode::ProgramsReplica,
            LifecycleModeArg::Ephemeral => LifecycleMode::Ephemeral,
            LifecycleModeArg::Offline => LifecycleMode::Offline,
        }
    }
}

#[derive(Parser, Debug, Clone, Copy)]
pub struct CommitStrategyArg {
    /// How often (in milliseconds) we should commit the accounts.
    #[arg(
        long = "accounts-commit-frequency-millis",
        default_value = "50",
        env = "ACCOUNTS_COMMIT_FREQUENCY_MILLIS"
    )]
    pub frequency_millis: u64,
    /// The compute unit price offered when we send the commit account transaction.
    /// This is in micro lamports and defaults to 1 Lamport.
    #[arg(
        long = "accounts-commit-compute-unit-price",
        default_value = "1000000",
        env = "ACCOUNTS_COMMIT_COMPUTE_UNIT_PRICE"
    )]
    pub compute_unit_price: u64,
}

impl Into<CommitStrategy> for CommitStrategyArg {
    fn into(self) -> CommitStrategy {
        CommitStrategy {
            frequency_millis: self.frequency_millis,
            compute_unit_price: self.compute_unit_price,
        }
    }
}

#[derive(Parser, Debug, Clone, Copy)]
struct PayerArgs {
    /// The payer init balance in lamports.
    #[arg(long = "accounts-payer-init-lamports", env = "INIT_LAMPORTS")]
    init_lamports: Option<u64>,
    /// The payer init balance in SOL.
    #[arg(long = "accounts-payer-init-sol")]
    init_sol: Option<u64>,
}

impl Into<Payer> for PayerArgs {
    fn into(self) -> Payer {
        Payer::new(PayerParams {
            init_lamports: self.init_lamports,
            init_sol: self.init_sol,
        })
    }
}

#[derive(Parser, Debug, Clone, Copy)]
struct AccountsDbArgs {
    /// Size of the main storage, we have to preallocate in advance. Default is 100MiB.
    #[arg(long = "accounts-db-size", default_value = "104857600")]
    pub db_size: usize,
    /// Minimal indivisible unit of addressing in main storage.
    /// Offsets are calculated in terms of blocks.
    #[arg(long = "accounts-db-block-size", default_value = "block256")]
    pub block_size: BlockSizeArg,
    /// Size of index file, we have to preallocate, can be 1% of main storage size. Default is 10MB.
    #[arg(long = "accounts-db-index-map-size", default_value = "10485760")]
    pub index_map_size: usize,
    /// Max number of snapshots to keep around.
    #[arg(long = "accounts-db-max-snapshots", default_value = "32")]
    pub max_snapshots: u16,
    /// How frequently (slot-wise) we should take snapshots.
    #[arg(long = "accounts-db-snapshot-frequency", default_value = "50")]
    pub snapshot_frequency: u64,
}

impl Into<AccountsDbConfig> for AccountsDbArgs {
    fn into(self) -> AccountsDbConfig {
        AccountsDbConfig {
            db_size: self.db_size,
            block_size: self.block_size.into(),
            index_map_size: self.index_map_size,
            max_snapshots: self.max_snapshots,
            snapshot_frequency: self.snapshot_frequency,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BlockSizeArg {
    Block128 = 128,
    #[default]
    Block256 = 256,
    Block512 = 512,
}

impl Into<BlockSize> for BlockSizeArg {
    fn into(self) -> BlockSize {
        match self {
            BlockSizeArg::Block128 => BlockSize::Block128,
            BlockSizeArg::Block256 => BlockSize::Block256,
            BlockSizeArg::Block512 => BlockSize::Block512,
        }
    }
}

#[derive(Parser, Debug, Clone, Copy)]
struct RpcArgs {
    /// The address to listen on
    #[arg(long = "rpc-addr", default_value = "0.0.0.0", env = "RPC_ADDR")]
    addr: IpAddr,
    /// The port to listen on
    #[arg(long = "rpc-port", default_value = "8899", env = "RPC_PORT")]
    port: u16,
}

impl Into<RpcConfig> for RpcArgs {
    fn into(self) -> RpcConfig {
        RpcConfig {
            addr: self.addr,
            port: self.port,
        }
    }
}

#[derive(Parser, Debug, Clone, Copy)]
struct GeyserGrpcArgs {
    /// The address to listen on
    #[arg(
        long = "geyser-grpc-addr",
        default_value = "0.0.0.0",
        env = "GEYSER_GRPC_ADDR"
    )]
    grpc_addr: IpAddr,
    /// The port to listen on
    #[arg(
        long = "geyser-grpc-port",
        default_value = "10000",
        env = "GEYSER_GRPC_PORT"
    )]
    grpc_port: u16,
}

impl Into<GeyserGrpcConfig> for GeyserGrpcArgs {
    fn into(self) -> GeyserGrpcConfig {
        GeyserGrpcConfig {
            addr: self.grpc_addr,
            port: self.grpc_port,
        }
    }
}

#[derive(Parser, Debug, Clone)]
pub struct ValidatorConfigArgs {
    /// Duration of a slot in milliseconds.
    #[arg(
        long = "validator-millis-per-slot",
        default_value = "50",
        env = "VALIDATOR_MILLIS_PER_SLOT"
    )]
    pub millis_per_slot: u64,

    /// Whether to verify transaction signatures.
    #[arg(
        long = "validator-sigverify",
        default_value = "true",
        env = "VALIDATOR_SIG_VERIFY"
    )]
    pub sigverify: bool,

    /// Fully Qualified Domain Name. If specified it will also register ER on chain
    #[arg(long = "validator-fdqn", env = "VALIDATOR_FDQN")]
    pub fdqn: Option<String>,

    /// Base fees for transactions.
    #[arg(long = "validator-base-fees", env = "VALIDATOR_BASE_FEES")]
    pub base_fees: Option<u64>,

    /// Uses alpha2 country codes following https://en.wikipedia.org/wiki/ISO_3166-1
    #[arg(
        long = "validator-country-code",
        default_value = "US",
        env = "VALIDATOR_COUNTRY_CODE",
        value_parser = country_code_parser
    )]
    pub country_code: CountryCode,
}

impl Into<ValidatorConfig> for ValidatorConfigArgs {
    fn into(self) -> ValidatorConfig {
        ValidatorConfig {
            country_code: self.country_code,
            base_fees: self.base_fees,
            fdqn: self.fdqn,
            millis_per_slot: self.millis_per_slot,
            sigverify: self.sigverify,
        }
    }
}

fn country_code_parser(s: &str) -> Result<CountryCode, String> {
    CountryCode::for_alpha2(s)
        .map_err(|e| format!("Invalid country code: {}", e))
}

#[derive(Parser, Debug, Clone)]
pub struct LedgerConfigArgs {
    /// Whether to remove a previous ledger if it exists.
    #[arg(long = "ledger-reset", default_value = "true", env = "LEDGER_RESET")]
    pub reset: bool,
    /// The file system path onto which the ledger should be written at.
    /// If left empty it will be auto-generated to a temporary folder
    #[arg(long = "ledger-path", env = "LEDGER_PATH")]
    pub path: Option<String>,
    /// The size under which it's desired to keep ledger in bytes. Default is 100 GiB
    #[arg(
        long = "ledger-size",
        default_value = "107374182400",
        env = "LEDGER_SIZE"
    )]
    pub size: u64,
}

impl Into<LedgerConfig> for LedgerConfigArgs {
    fn into(self) -> LedgerConfig {
        LedgerConfig {
            reset: self.reset,
            path: self.path,
            size: self.size,
        }
    }
}

fn program_config_parser(s: &str) -> Result<ProgramConfig, String> {
    let parts: Vec<String> =
        s.split(':').map(|part| part.to_string()).collect();
    let [id, path] = parts.as_slice() else {
        return Err(format!("Invalid program config: {}", s));
    };
    let id = Pubkey::from_str(&id)
        .map_err(|e| format!("Invalid program id {}: {}", id, e))?;

    Ok(ProgramConfig {
        id,
        path: path.clone(),
    })
}

#[derive(Parser, Debug)]
pub struct MetricsConfig {
    /// Whether to enable metrics.
    #[arg(
        long = "metrics-enabled",
        default_value = "true",
        env = "METRICS_ENABLED"
    )]
    pub enabled: bool,
    /// The interval in seconds at which system metrics are collected.
    #[arg(
        long = "metrics-system-metrics-tick-interval-secs",
        default_value = "30",
        env = "METRICS_SYSTEM_METRICS_TICK_INTERVAL_SECS"
    )]
    pub system_metrics_tick_interval_secs: u64,
    /// The address to listen on
    #[arg(
        long = "metrics-addr",
        default_value = "0.0.0.0",
        env = "METRICS_ADDR"
    )]
    pub metrics_addr: IpAddr,
    /// The port to listen on
    #[arg(long = "metrics-port", default_value = "9000", env = "METRICS_PORT")]
    pub metrics_port: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_CONFIG_PATH: &str = "path/to/my/config.toml";

    fn set_env_var<'a>(name: &'a str, value: &'a str) -> &'a str {
        std::env::set_var(name, value);
        value
    }

    #[test]
    fn test_env_vars_override_config() {
        let validator_keypair =
            set_env_var("VALIDATOR_KEYPAIR", "path/to/my/keypair.json");
        let geyser_disable = set_env_var("GEYSER_DISABLE", "(accounts)");
        let geyser_cache_disable =
            set_env_var("GEYSER_CACHE_DISABLE", "(accounts)");
        let remote_custom =
            set_env_var("ACCOUNTS_REMOTE", "https://example.com");
        let remote_custom_with_ws =
            set_env_var("ACCOUNTS_REMOTE_WS", "wss://example.com");
        let accounts_lifecycle = set_env_var("ACCOUNTS_LIFECYCLE", "offline");
        let accounts_commit_frequency_millis =
            set_env_var("ACCOUNTS_COMMIT_FREQUENCY_MILLIS", "50");
        let accounts_commit_compute_unit_price =
            set_env_var("ACCOUNTS_COMMIT_COMPUTE_UNIT_PRICE", "100");
        let init_lamports = set_env_var("INIT_LAMPORTS", "1000");
        let ledger_reset = set_env_var("LEDGER_RESET", "false");
        let ledger_path = set_env_var("LEDGER_PATH", "path/to/my/ledger");
        let ledger_size = set_env_var("LEDGER_SIZE", "12400");
        let validator_millis_per_slot =
            set_env_var("VALIDATOR_MILLIS_PER_SLOT", "50");
        let validator_sig_verify = set_env_var("VALIDATOR_SIG_VERIFY", "true");
        let validator_base_fees = set_env_var("VALIDATOR_BASE_FEES", "1000000");
        let validator_fdqn = set_env_var("VALIDATOR_FDQN", "example.com");
        let validator_country_code =
            set_env_var("VALIDATOR_COUNTRY_CODE", "AU");
        let metrics_enabled = set_env_var("METRICS_ENABLED", "true");
        let metrics_addr = set_env_var("METRICS_ADDR", "0.0.0.0");
        let metrics_port = set_env_var("METRICS_PORT", "9000");
        let metrics_system_metrics_tick_interval_secs =
            set_env_var("METRICS_SYSTEM_METRICS_TICK_INTERVAL_SECS", "30");

        let args: Vec<&str> = vec![];
        let cli = Cli::try_parse_from(&args).unwrap();
        let config = cli
            .config
            .override_config(EphemeralConfig::default())
            .unwrap();
        assert_eq!(cli.keypair, Some(validator_keypair.to_string()));
        assert_eq!(cli.disable_geyser, Some(geyser_disable.to_string()));
        assert_eq!(
            cli.disable_geyser_cache,
            Some(geyser_cache_disable.to_string())
        );
        assert_eq!(
            config.accounts.remote,
            RemoteConfig::CustomWithWs(
                Url::from_str(&remote_custom).unwrap(),
                Url::from_str(&remote_custom_with_ws).unwrap()
            )
        );
        assert_eq!(
            config.accounts.lifecycle,
            LifecycleModeArg::from_str(accounts_lifecycle, true)
                .unwrap()
                .into()
        );
        assert_eq!(
            config.accounts.commit.frequency_millis,
            accounts_commit_frequency_millis.parse::<u64>().unwrap()
        );
        assert_eq!(
            config.accounts.commit.compute_unit_price,
            accounts_commit_compute_unit_price.parse::<u64>().unwrap()
        );
        assert_eq!(
            config.accounts.payer.init_lamports,
            Some(init_lamports.parse::<u64>().unwrap())
        );
        assert_eq!(config.ledger.reset, ledger_reset.parse::<bool>().unwrap());
        assert_eq!(config.ledger.path, Some(ledger_path.to_string()));
        assert_eq!(config.ledger.size, ledger_size.parse::<u64>().unwrap());
        assert_eq!(config.validator.fdqn, Some(validator_fdqn.to_string()));
        assert_eq!(
            config.validator.millis_per_slot,
            validator_millis_per_slot.parse::<u64>().unwrap()
        );
        assert_eq!(
            config.validator.sigverify,
            validator_sig_verify.parse::<bool>().unwrap()
        );
        assert_eq!(
            config.validator.base_fees,
            Some(validator_base_fees.parse::<u64>().unwrap())
        );
        assert_eq!(
            config.validator.country_code.alpha2(),
            validator_country_code
        );
        assert_eq!(
            config.metrics.enabled,
            metrics_enabled.parse::<bool>().unwrap()
        );
        assert_eq!(
            config.metrics.service.addr,
            metrics_addr.parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            config.metrics.service.port,
            metrics_port.parse::<u16>().unwrap()
        );
        assert_eq!(
            config.metrics.system_metrics_tick_interval_secs,
            metrics_system_metrics_tick_interval_secs
                .parse::<u64>()
                .unwrap()
        );
    }

    #[test]
    fn test_accounts_remote_custom_with_ws() {
        let cli = Cli::try_parse_from(&[
            DEFAULT_CONFIG_PATH,
            "--accounts-remote-custom-with-ws",
            "wss://example.com",
        ]);
        assert!(cli.is_err());

        let cli = Cli::try_parse_from(&[
            DEFAULT_CONFIG_PATH,
            "--accounts-remote-custom",
            "https://example.com",
            "--accounts-remote-custom-with-ws",
            "wss://example.com",
        ]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_parse_programs() {
        let cli = Cli::parse_from(&[
            DEFAULT_CONFIG_PATH,
            "--programs",
            "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev:path1",
            "--programs",
            "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev:path2",
        ]);
        assert_eq!(
            cli.config.programs,
            vec![
                ProgramConfig {
                    id: Pubkey::from_str_const(
                        "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev"
                    ),
                    path: "path1".to_string()
                },
                ProgramConfig {
                    id: Pubkey::from_str_const(
                        "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev"
                    ),
                    path: "path2".to_string()
                }
            ]
        )
    }
}
