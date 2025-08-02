use std::{fmt, fs, path::PathBuf, str::FromStr};

use clap::Args;
use errors::{ConfigError, ConfigResult};
use magicblock_config_macro::Mergeable;
use serde::{Deserialize, Serialize};
use solana_pubkey::Pubkey;

mod accounts;
mod accounts_db;
mod cli;
pub mod errors;
mod geyser_grpc;
mod helpers;
mod ledger;
mod metrics;
mod program;
mod rpc;
mod validator;
pub use accounts::*;
pub use accounts_db::*;
pub use cli::*;
pub use geyser_grpc::*;
pub use ledger::*;
pub use metrics::*;
pub use program::*;
pub use rpc::*;
pub use validator::*;

#[derive(
    Debug,
    Default,
    Clone,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    Args,
    Mergeable,
)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct EphemeralConfig {
    #[serde(default)]
    #[command(flatten)]
    pub accounts: AccountsConfig,
    #[serde(default)]
    #[command(flatten)]
    pub rpc: RpcConfig,
    #[serde(default)]
    #[command(flatten)]
    pub geyser_grpc: GeyserGrpcConfig,
    #[serde(default)]
    #[command(flatten)]
    pub validator: ValidatorConfig,
    #[serde(default)]
    #[command(flatten)]
    pub ledger: LedgerConfig,
    #[serde(default)]
    #[serde(rename = "program")]
    #[arg(
        long,
        help = "The list of programs to load. Format: <program_id>:<path_to_program_binary>",
        value_parser = program_config_parser,
    )]
    pub programs: Vec<ProgramConfig>,
    #[serde(default)]
    #[command(flatten)]
    pub metrics: MetricsConfig,
}

impl EphemeralConfig {
    pub fn try_load_from_file(path: &PathBuf) -> ConfigResult<Self> {
        let toml = fs::read_to_string(path)?;
        Self::try_load_from_toml(&toml, Some(path))
    }

    pub fn try_load_from_toml(
        toml: &str,
        config_path: Option<&PathBuf>,
    ) -> ConfigResult<Self> {
        let mut config: Self = toml::from_str(toml)?;
        for program in &mut config.programs {
            // If we know the config path we can resolve relative program paths
            // Otherwise they have to be absolute. However if no config path was
            // provided this usually means that we are provided some default toml
            // config file which doesn't include any program paths.
            if let Some(config_path) = config_path {
                program.path = config_path
                    .parent()
                    .ok_or_else(|| {
                        ConfigError::ConfigPathInvalid(format!(
                            "Config path: '{}' is missing parent dir",
                            config_path.display()
                        ))
                    })?
                    .join(&program.path)
                    .to_str()
                    .ok_or_else(|| {
                        ConfigError::ProgramPathInvalidUnicode(
                            program.id.to_string(),
                            program.path.to_string(),
                        )
                    })?
                    .to_string()
            }
        }

        config.post_parse();
        config
            .ledger
            .resume_strategy_config
            .validate_resume_strategy()?;

        Ok(config)
    }

    pub fn post_parse(&mut self) {
        if self.accounts.remote.url.is_some() {
            match &self.accounts.remote.ws_url {
                Some(ws_url) if ws_url.len() > 1 => {
                    self.accounts.remote.cluster =
                        RemoteCluster::CustomWithMultipleWs;
                }
                Some(ws_url) if ws_url.len() == 1 => {
                    self.accounts.remote.cluster = RemoteCluster::CustomWithWs;
                }
                _ => {
                    self.accounts.remote.cluster = RemoteCluster::Custom;
                }
            }
        }
    }
}

impl fmt::Display for EphemeralConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let toml = toml::to_string_pretty(self)
            .unwrap_or("Invalid Config".to_string());
        write!(f, "{toml}")
    }
}

fn program_config_parser(s: &str) -> Result<ProgramConfig, String> {
    let parts: Vec<String> =
        s.split(':').map(|part| part.to_string()).collect();
    let [id, path] = parts.as_slice() else {
        return Err(format!("Invalid program config: {s}"));
    };
    let id = Pubkey::from_str(id)
        .map_err(|e| format!("Invalid program id {id}: {e}"))?;

    Ok(ProgramConfig {
        id,
        path: path.clone(),
    })
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::Pubkey;
    use isocountry::CountryCode;
    use url::Url;

    use super::*;

    #[test]
    fn test_program_config_parser() {
        let config = program_config_parser(
            "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev:path1",
        )
        .unwrap();
        assert_eq!(
            config.id,
            Pubkey::from_str_const(
                "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev"
            )
        );
        assert_eq!(config.path, "path1");
    }

    #[test]
    fn test_post_parse() {
        let mut config = EphemeralConfig::default();
        config.accounts.remote.url =
            Some(Url::parse("https://validator.example.com").unwrap());
        config.accounts.remote.ws_url =
            Some(vec![Url::parse("wss://validator.example.com").unwrap()]);
        assert_eq!(config.accounts.remote.cluster, RemoteCluster::Devnet);

        config.post_parse();

        assert_eq!(config.accounts.remote.cluster, RemoteCluster::CustomWithWs);
    }

    #[test]
    fn test_merge_with_default() {
        let mut config = EphemeralConfig {
            accounts: AccountsConfig {
                remote: RemoteConfig {
                    cluster: RemoteCluster::CustomWithWs,
                    url: Some(
                        Url::parse("https://validator.example.com").unwrap(),
                    ),
                    ws_url: Some(vec![Url::parse(
                        "wss://validator.example.com",
                    )
                    .unwrap()]),
                },
                lifecycle: LifecycleMode::Offline,
                commit: CommitStrategyConfig {
                    frequency_millis: 123,
                    compute_unit_price: 123,
                },
                allowed_programs: vec![AllowedProgram {
                    id: Pubkey::from_str_const(
                        "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev",
                    ),
                }],
                db: AccountsDbConfig {
                    db_size: 1000000000,
                    block_size: BlockSize::Block128,
                    index_map_size: 1000000000,
                    max_snapshots: 1234,
                    snapshot_frequency: 1000000000,
                },
                clone: AccountsCloneConfig {
                    prepare_lookup_tables: PrepareLookupTables::Always,
                    auto_airdrop_lamports: 123,
                },
                max_monitored_accounts: 1234,
            },
            rpc: RpcConfig {
                addr: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 127)),
                port: 9090,
                max_ws_connections: 8008,
            },
            geyser_grpc: GeyserGrpcConfig {
                addr: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 127)),
                port: 9090,
            },
            validator: ValidatorConfig {
                millis_per_slot: 5000,
                sigverify: false,
                fqdn: Some("validator.example.com".to_string()),
                base_fees: Some(1000000000),
                country_code: CountryCode::for_alpha2("FR").unwrap(),
                claim_fees_interval_secs: DEFAULT_CLAIM_FEES_INTERVAL_SECS,
            },
            ledger: LedgerConfig {
                resume_strategy_config: LedgerResumeStrategyConfig {
                    kind: LedgerResumeStrategyType::Replay,
                    reset_slot: Some(1),
                    keep_accounts: Some(true),
                    account_hydration_concurrency: 20,
                },
                skip_keypair_match_check: true,
                path: "ledger.example.com".to_string(),
                size: 1000000000,
            },
            programs: vec![ProgramConfig {
                id: Pubkey::from_str_const(
                    "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev",
                ),
                path: "path1".to_string(),
            }],
            metrics: MetricsConfig {
                enabled: true,
                system_metrics_tick_interval_secs: 321,
                service: MetricsServiceConfig {
                    addr: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 127)),
                    port: 9090,
                },
            },
        };
        let original_config = config.clone();
        let other = EphemeralConfig::default();

        config.merge(other);

        assert_eq!(config, original_config);
    }

    #[test]
    fn test_merge_default_with_non_default() {
        let mut config = EphemeralConfig::default();
        let other = EphemeralConfig {
            accounts: AccountsConfig {
                remote: RemoteConfig {
                    cluster: RemoteCluster::CustomWithWs,
                    url: Some(
                        Url::parse("https://validator.example.com").unwrap(),
                    ),
                    ws_url: Some(vec![Url::parse(
                        "wss://validator.example.com",
                    )
                    .unwrap()]),
                },
                lifecycle: LifecycleMode::Offline,
                commit: CommitStrategyConfig {
                    frequency_millis: 123,
                    compute_unit_price: 123,
                },
                allowed_programs: vec![AllowedProgram {
                    id: Pubkey::from_str_const(
                        "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev",
                    ),
                }],
                db: AccountsDbConfig {
                    db_size: 1000000000,
                    block_size: BlockSize::Block128,
                    index_map_size: 1000000000,
                    max_snapshots: 12345,
                    snapshot_frequency: 1000000000,
                },
                clone: AccountsCloneConfig {
                    prepare_lookup_tables: PrepareLookupTables::Always,
                    auto_airdrop_lamports: 123,
                },
                max_monitored_accounts: 1234,
            },
            rpc: RpcConfig {
                addr: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 127)),
                port: 9090,
                max_ws_connections: 8008,
            },
            geyser_grpc: GeyserGrpcConfig {
                addr: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 127)),
                port: 9090,
            },
            validator: ValidatorConfig {
                millis_per_slot: 5000,
                sigverify: false,
                fqdn: Some("validator.example.com".to_string()),
                base_fees: Some(1000000000),
                country_code: CountryCode::for_alpha2("FR").unwrap(),
                claim_fees_interval_secs: DEFAULT_CLAIM_FEES_INTERVAL_SECS,
            },
            ledger: LedgerConfig {
                resume_strategy_config: LedgerResumeStrategyConfig {
                    kind: LedgerResumeStrategyType::Replay,
                    reset_slot: Some(1),
                    keep_accounts: Some(true),
                    account_hydration_concurrency: 20,
                },
                skip_keypair_match_check: true,
                path: "ledger.example.com".to_string(),
                size: 1000000000,
            },
            programs: vec![ProgramConfig {
                id: Pubkey::from_str_const(
                    "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev",
                ),
                path: "path1".to_string(),
            }],
            metrics: MetricsConfig {
                enabled: true,
                system_metrics_tick_interval_secs: 321,
                service: MetricsServiceConfig {
                    addr: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 127)),
                    port: 9090,
                },
            },
        };

        config.merge(other.clone());

        assert_eq!(config, other);
    }

    #[test]
    fn test_merge_non_default() {
        let mut config = EphemeralConfig {
            accounts: AccountsConfig {
                remote: RemoteConfig {
                    cluster: RemoteCluster::CustomWithWs,
                    url: Some(
                        Url::parse("https://validator2.example.com").unwrap(),
                    ),
                    ws_url: Some(vec![Url::parse(
                        "wss://validator2.example.com",
                    )
                    .unwrap()]),
                },
                lifecycle: LifecycleMode::Offline,
                commit: CommitStrategyConfig {
                    frequency_millis: 12365,
                    compute_unit_price: 123665,
                },
                allowed_programs: vec![AllowedProgram {
                    id: Pubkey::from_str_const(
                        "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev",
                    ),
                }],
                db: AccountsDbConfig {
                    db_size: 999,
                    block_size: BlockSize::Block128,
                    index_map_size: 999,
                    max_snapshots: 12345,
                    snapshot_frequency: 999,
                },
                clone: AccountsCloneConfig {
                    prepare_lookup_tables: PrepareLookupTables::Always,
                    auto_airdrop_lamports: 123,
                },
                max_monitored_accounts: 12346,
            },
            rpc: RpcConfig {
                addr: IpAddr::V4(Ipv4Addr::new(1, 0, 0, 127)),
                port: 9091,
                max_ws_connections: 8008,
            },
            geyser_grpc: GeyserGrpcConfig {
                addr: IpAddr::V4(Ipv4Addr::new(1, 0, 0, 127)),
                port: 9091,
            },
            validator: ValidatorConfig {
                millis_per_slot: 5001,
                sigverify: false,
                fqdn: Some("validator2.example.com".to_string()),
                base_fees: Some(9999),
                country_code: CountryCode::for_alpha2("DE").unwrap(),
                claim_fees_interval_secs: DEFAULT_CLAIM_FEES_INTERVAL_SECS,
            },
            ledger: LedgerConfig {
                resume_strategy_config: LedgerResumeStrategyConfig {
                    kind: LedgerResumeStrategyType::ResumeOnly,
                    reset_slot: Some(1),
                    keep_accounts: Some(true),
                    account_hydration_concurrency: 20,
                },
                skip_keypair_match_check: true,
                path: "ledger2.example.com".to_string(),
                size: 100000,
            },
            programs: vec![ProgramConfig {
                id: Pubkey::from_str_const(
                    "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev",
                ),
                path: "path1".to_string(),
            }],
            metrics: MetricsConfig {
                enabled: true,
                system_metrics_tick_interval_secs: 3210,
                service: MetricsServiceConfig {
                    addr: IpAddr::V4(Ipv4Addr::new(1, 0, 0, 127)),
                    port: 9090,
                },
            },
        };
        let original_config = config.clone();
        let other = EphemeralConfig {
            accounts: AccountsConfig {
                remote: RemoteConfig {
                    cluster: RemoteCluster::CustomWithWs,
                    url: Some(
                        Url::parse("https://validator.example.com").unwrap(),
                    ),
                    ws_url: Some(vec![Url::parse(
                        "wss://validator.example.com",
                    )
                    .unwrap()]),
                },
                lifecycle: LifecycleMode::Offline,
                commit: CommitStrategyConfig {
                    frequency_millis: 123,
                    compute_unit_price: 123,
                },
                allowed_programs: vec![AllowedProgram {
                    id: Pubkey::from_str_const(
                        "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev",
                    ),
                }],
                db: AccountsDbConfig {
                    db_size: 1000000000,
                    block_size: BlockSize::Block128,
                    index_map_size: 1000000000,
                    max_snapshots: 12345,
                    snapshot_frequency: 1000000000,
                },
                clone: AccountsCloneConfig {
                    prepare_lookup_tables: PrepareLookupTables::Always,
                    auto_airdrop_lamports: 12345,
                },
                max_monitored_accounts: 1234,
            },
            rpc: RpcConfig {
                addr: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 127)),
                port: 9090,
                max_ws_connections: 8008,
            },
            geyser_grpc: GeyserGrpcConfig {
                addr: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 127)),
                port: 9090,
            },
            validator: ValidatorConfig {
                millis_per_slot: 5000,
                sigverify: false,
                fqdn: Some("validator.example.com".to_string()),
                base_fees: Some(1000000000),
                country_code: CountryCode::for_alpha2("FR").unwrap(),
                claim_fees_interval_secs: DEFAULT_CLAIM_FEES_INTERVAL_SECS,
            },
            ledger: LedgerConfig {
                resume_strategy_config: LedgerResumeStrategyConfig {
                    kind: LedgerResumeStrategyType::Replay,
                    reset_slot: Some(2),
                    keep_accounts: Some(false),
                    account_hydration_concurrency: 20,
                },
                skip_keypair_match_check: true,
                path: "ledger.example.com".to_string(),
                size: 1000000000,
            },
            programs: vec![ProgramConfig {
                id: Pubkey::from_str_const(
                    "mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev",
                ),
                path: "path1".to_string(),
            }],
            metrics: MetricsConfig {
                enabled: true,
                system_metrics_tick_interval_secs: 321,
                service: MetricsServiceConfig {
                    addr: IpAddr::V4(Ipv4Addr::new(0, 0, 0, 127)),
                    port: 9090,
                },
            },
        };

        config.merge(other);

        assert_eq!(config, original_config);
    }

    #[test]
    fn test_accounts_clone_config_always() {
        let mut config = EphemeralConfig::default();
        let other = EphemeralConfig {
            accounts: AccountsConfig {
                remote: RemoteConfig {
                    cluster: RemoteCluster::Devnet,
                    url: None,
                    ws_url: None,
                },
                lifecycle: LifecycleMode::Offline,
                commit: CommitStrategyConfig {
                    frequency_millis: 9_000_000_000_000,
                    compute_unit_price: 1_000_000,
                },
                allowed_programs: vec![],
                db: AccountsDbConfig::default(),
                clone: AccountsCloneConfig {
                    prepare_lookup_tables: PrepareLookupTables::Always,
                    auto_airdrop_lamports: 0,
                },
                max_monitored_accounts: 2048,
            },
            rpc: RpcConfig::default(),
            geyser_grpc: GeyserGrpcConfig::default(),
            validator: ValidatorConfig::default(),
            ledger: LedgerConfig {
                resume_strategy_config: LedgerResumeStrategyConfig {
                    kind: LedgerResumeStrategyType::Replay,
                    reset_slot: Some(1),
                    keep_accounts: Some(true),
                    account_hydration_concurrency: 20,
                },
                skip_keypair_match_check: true,
                path: "ledger.example.com".to_string(),
                size: 1000000000,
            },
            programs: vec![],
            metrics: MetricsConfig::default(),
        };

        config.merge(other.clone());

        assert_eq!(config, other);
        // Test that the clone config is properly set to Always
        assert_eq!(
            config.accounts.clone.prepare_lookup_tables,
            PrepareLookupTables::Always
        );
    }
}
