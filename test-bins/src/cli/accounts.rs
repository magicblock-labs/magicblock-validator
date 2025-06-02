use clap::{Parser, ValueEnum};
use magicblock_accounts_db::config::{AccountsDbConfig, BlockSize};
use magicblock_config::{CommitStrategy, LifecycleMode, Payer, PayerParams};

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
    #[arg(
        long = "accounts-lifecycle",
        default_value = "programs-replica",
        env = "ACCOUNTS_LIFECYCLE"
    )]
    pub lifecycle: LifecycleModeArg,
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

impl From<CommitStrategyArg> for CommitStrategy {
    fn from(value: CommitStrategyArg) -> Self {
        CommitStrategy {
            frequency_millis: value.frequency_millis,
            compute_unit_price: value.compute_unit_price,
        }
    }
}

#[derive(Parser, Debug, Clone, Copy)]
pub struct PayerArgs {
    /// The payer init balance in lamports.
    #[arg(long = "accounts-payer-init-lamports", env = "INIT_LAMPORTS")]
    init_lamports: Option<u64>,
    /// The payer init balance in SOL.
    #[arg(long = "accounts-payer-init-sol")]
    init_sol: Option<u64>,
}

impl From<PayerArgs> for Payer {
    fn from(value: PayerArgs) -> Self {
        Payer::new(PayerParams {
            init_lamports: value.init_lamports,
            init_sol: value.init_sol,
        })
    }
}

#[derive(Parser, Debug, Clone, Copy)]
pub struct AccountsDbArgs {
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

impl From<AccountsDbArgs> for AccountsDbConfig {
    fn from(value: AccountsDbArgs) -> Self {
        AccountsDbConfig {
            db_size: value.db_size,
            block_size: value.block_size.into(),
            index_map_size: value.index_map_size,
            max_snapshots: value.max_snapshots,
            snapshot_frequency: value.snapshot_frequency,
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

impl From<BlockSizeArg> for BlockSize {
    fn from(value: BlockSizeArg) -> Self {
        match value {
            BlockSizeArg::Block128 => BlockSize::Block128,
            BlockSizeArg::Block256 => BlockSize::Block256,
            BlockSizeArg::Block512 => BlockSize::Block512,
        }
    }
}
