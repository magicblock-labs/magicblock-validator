use std::str::FromStr;

use clap::{Parser, ValueEnum};
use magicblock_accounts_db::config::{AccountsDbConfig, BlockSize};
use magicblock_api::EphemeralConfig;
use magicblock_config::{
    AllowedProgram, CommitStrategy, LifecycleMode, Payer, PayerParams,
    RemoteConfig,
};
use solana_sdk::pubkey::Pubkey;
use url::Url;

#[derive(Parser, Debug)]
pub struct AccountsArgs {
    /// Use a predefined remote configuration
    #[arg(
        long = "accounts-remote",
        conflicts_with = "remote_custom",
        conflicts_with = "remote_custom_with_ws"
    )]
    pub remote: Option<RemoteConfigArg>,
    /// Use a custom remote URL
    #[arg(
        long = "accounts-remote-custom",
        env = "ACCOUNTS_REMOTE",
        conflicts_with = "remote"
    )]
    pub remote_custom: Option<String>,
    /// Use a custom remote URL with WebSocket URL
    #[arg(
        long = "accounts-remote-custom-with-ws",
        env = "ACCOUNTS_REMOTE_WS",
        conflicts_with = "remote",
        requires = "remote_custom"
    )]
    pub remote_custom_with_ws: Option<String>,
    #[arg(long = "accounts-lifecycle", env = "ACCOUNTS_LIFECYCLE")]
    pub lifecycle: Option<LifecycleModeArg>,
    #[command(flatten)]
    pub commit: CommitStrategyArg,
    #[command(flatten)]
    pub payer: PayerArgs,
    /// List of allowed programs.
    #[arg(long = "accounts-allowed-programs")]
    pub allowed_programs: Vec<String>,
    #[command(flatten)]
    pub db: AccountsDbArgs,
}

impl AccountsArgs {
    pub fn merge_with_config(
        &self,
        config: &mut EphemeralConfig,
    ) -> Result<(), String> {
        if let Some(remote) = self.remote {
            config.accounts.remote = match remote {
                RemoteConfigArg::Devnet => RemoteConfig::Devnet,
                RemoteConfigArg::Mainnet => RemoteConfig::Mainnet,
                RemoteConfigArg::Testnet => RemoteConfig::Testnet,
                RemoteConfigArg::Development => RemoteConfig::Development,
            };
        } else if let Some(remote_custom) = &self.remote_custom {
            let url = Url::from_str(remote_custom).map_err(|e| {
                format!("Failed to parse URL {}: {}", remote_custom, e)
            })?;

            if let Some(remote_custom_with_ws) = &self.remote_custom_with_ws {
                let url_ws =
                    Url::from_str(remote_custom_with_ws).map_err(|e| {
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

        config.accounts.lifecycle = self
            .lifecycle
            .unwrap_or(config.accounts.lifecycle.into())
            .into();

        config.accounts.commit = CommitStrategy {
            frequency_millis: self
                .commit
                .frequency_millis
                .unwrap_or(config.accounts.commit.frequency_millis),
            compute_unit_price: self
                .commit
                .compute_unit_price
                .unwrap_or(config.accounts.commit.compute_unit_price),
        };

        if self.payer.init_lamports.is_some() {
            config.accounts.payer = Payer::new(PayerParams {
                init_lamports: self.payer.init_lamports,
                init_sol: None,
            })
        } else if self.payer.init_sol.is_some() {
            config.accounts.payer = Payer::new(PayerParams {
                init_lamports: None,
                init_sol: self.payer.init_sol,
            })
        }

        config.accounts.allowed_programs.extend(
            self.allowed_programs.clone().into_iter().map(|id| {
                AllowedProgram {
                    id: Pubkey::from_str_const(&id),
                }
            }),
        );

        config.accounts.db = AccountsDbConfig {
            db_size: self.db.db_size.unwrap_or(config.accounts.db.db_size),
            block_size: self
                .db
                .block_size
                .unwrap_or(config.accounts.db.block_size.into())
                .into(),
            index_map_size: self
                .db
                .index_map_size
                .unwrap_or(config.accounts.db.index_map_size),
            max_snapshots: self
                .db
                .max_snapshots
                .unwrap_or(config.accounts.db.max_snapshots),
            snapshot_frequency: self
                .db
                .snapshot_frequency
                .unwrap_or(config.accounts.db.snapshot_frequency),
        };

        Ok(())
    }
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

impl From<LifecycleModeArg> for LifecycleMode {
    fn from(value: LifecycleModeArg) -> Self {
        match value {
            LifecycleModeArg::Replica => LifecycleMode::Replica,
            LifecycleModeArg::ProgramsReplica => LifecycleMode::ProgramsReplica,
            LifecycleModeArg::Ephemeral => LifecycleMode::Ephemeral,
            LifecycleModeArg::Offline => LifecycleMode::Offline,
        }
    }
}

impl From<LifecycleMode> for LifecycleModeArg {
    fn from(value: LifecycleMode) -> Self {
        match value {
            LifecycleMode::Replica => LifecycleModeArg::Replica,
            LifecycleMode::ProgramsReplica => LifecycleModeArg::ProgramsReplica,
            LifecycleMode::Ephemeral => LifecycleModeArg::Ephemeral,
            LifecycleMode::Offline => LifecycleModeArg::Offline,
        }
    }
}

#[derive(Parser, Debug, Clone, Copy)]
pub struct CommitStrategyArg {
    /// How often (in milliseconds) we should commit the accounts.
    #[arg(
        long = "accounts-commit-frequency-millis",
        env = "ACCOUNTS_COMMIT_FREQUENCY_MILLIS"
    )]
    pub frequency_millis: Option<u64>,
    /// The compute unit price offered when we send the commit account transaction.
    /// This is in micro lamports and defaults to 1 Lamport.
    #[arg(
        long = "accounts-commit-compute-unit-price",
        env = "ACCOUNTS_COMMIT_COMPUTE_UNIT_PRICE"
    )]
    pub compute_unit_price: Option<u64>,
}

#[derive(Parser, Debug, Clone, Copy)]
pub struct PayerArgs {
    /// The payer init balance in lamports.
    #[arg(
        long = "accounts-payer-init-lamports",
        env = "INIT_LAMPORTS",
        conflicts_with = "init_sol"
    )]
    init_lamports: Option<u64>,
    /// The payer init balance in SOL.
    #[arg(long = "accounts-payer-init-sol", conflicts_with = "init_lamports")]
    init_sol: Option<u64>,
}

#[derive(Parser, Debug, Clone, Copy)]
pub struct AccountsDbArgs {
    /// Size of the main storage, we have to preallocate in advance. Default is 100MiB.
    #[arg(long = "accounts-db-size")]
    pub db_size: Option<usize>,
    /// Minimal indivisible unit of addressing in main storage.
    /// Offsets are calculated in terms of blocks.
    #[arg(long = "accounts-db-block-size")]
    pub block_size: Option<BlockSizeArg>,
    /// Size of index file, we have to preallocate, can be 1% of main storage size. Default is 10MB.
    #[arg(long = "accounts-db-index-map-size")]
    pub index_map_size: Option<usize>,
    /// Max number of snapshots to keep around.
    #[arg(long = "accounts-db-max-snapshots")]
    pub max_snapshots: Option<u16>,
    /// How frequently (slot-wise) we should take snapshots.
    #[arg(long = "accounts-db-snapshot-frequency")]
    pub snapshot_frequency: Option<u64>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BlockSizeArg {
    Block128 = 128,
    #[default]
    Block256 = 256,
    Block512 = 512,
}

impl From<BlockSizeArg> for BlockSize {
    fn from(value: BlockSizeArg) -> Self {
        match value {
            BlockSizeArg::Block128 => BlockSize::Block128,
            BlockSizeArg::Block256 => BlockSize::Block256,
            BlockSizeArg::Block512 => BlockSize::Block512,
        }
    }
}

impl From<BlockSize> for BlockSizeArg {
    fn from(value: BlockSize) -> Self {
        match value {
            BlockSize::Block128 => BlockSizeArg::Block128,
            BlockSize::Block256 => BlockSizeArg::Block256,
            BlockSize::Block512 => BlockSizeArg::Block512,
        }
    }
}
