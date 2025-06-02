use clap::Parser;

mod accounts;
mod config;
mod geyser;
mod ledger;
mod metrics;
mod rpc;
mod validator;

use accounts::*;
use config::*;
use geyser::*;
use ledger::*;
use metrics::*;
use rpc::*;
use validator::*;

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

#[cfg(test)]
mod tests {
    use std::{net::IpAddr, str::FromStr};

    use clap::ValueEnum;
    use magicblock_api::EphemeralConfig;
    use magicblock_config::{ProgramConfig, RemoteConfig};
    use serial_test::serial;
    use solana_sdk::pubkey::Pubkey;
    use url::Url;

    use super::*;

    const DEFAULT_CONFIG_PATH: &str = "path/to/my/config.toml";

    fn set_env_var<'a>(name: &'a str, value: &'a str) -> &'a str {
        std::env::set_var(name, value);
        value
    }

    #[test]
    #[serial]
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
                Url::from_str(remote_custom).unwrap(),
                Url::from_str(remote_custom_with_ws).unwrap()
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
        assert_eq!(config.programs, vec![]);
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
    #[serial]
    fn test_accounts_remote_custom_with_ws() {
        // Prevent conflicts with other tests
        std::env::remove_var("ACCOUNTS_REMOTE");
        std::env::remove_var("ACCOUNTS_REMOTE_CUSTOM");
        std::env::remove_var("ACCOUNTS_REMOTE_CUSTOM_WITH_WS");

        let cli = Cli::try_parse_from([
            DEFAULT_CONFIG_PATH,
            "--accounts-remote-custom-with-ws",
            "wss://example.com",
        ]);
        assert!(cli.is_err());

        let cli = Cli::try_parse_from([
            DEFAULT_CONFIG_PATH,
            "--accounts-remote-custom",
            "https://example.com",
            "--accounts-remote-custom-with-ws",
            "wss://example.com",
        ]);
        assert!(cli.is_ok());
    }

    #[test]
    #[serial]
    fn test_parse_programs() {
        let cli = Cli::parse_from([
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
