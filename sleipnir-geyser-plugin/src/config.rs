// Adapted from yellowstone-grpc/yellowstone-grpc-geyser/src/config.rs
use {
    solana_sdk::pubkey::Pubkey,
    std::{
        collections::HashSet,
        net::{IpAddr, Ipv4Addr, SocketAddr},
    },
    tokio::sync::Semaphore,
};

#[derive(Debug, Default, Clone)]
pub struct Config {
    pub grpc: ConfigGrpc,
    /// Action on block re-construction error
    pub block_fail_action: ConfigBlockFailAction,
}

#[derive(Debug, Clone)]
pub struct ConfigGrpc {
    /// Address of Grpc service.
    pub address: SocketAddr,
    /// Limits the maximum size of a decoded message, default is 4MiB
    pub max_decoding_message_size: usize,
    /// Capacity of the channel per connection
    pub channel_capacity: usize,
    /// Concurrency limit for unary requests
    pub unary_concurrency_limit: usize,
    /// Enable/disable unary methods
    pub unary_disabled: bool,
    /// Limits for possible filters
    pub filters: ConfigGrpcFilters,
    /// Normalizes filter commitment levels to 'processed' no matter
    /// what actual commitment level was passed by the user
    pub normalize_commitment_level: bool,
}

const MAX_DECODING_MESSAGE_SIZE_DEFAULT: usize = 4 * 1024 * 1024;
const CHANNEL_CAPACITY_DEFAULT: usize = 250_000;
const UNARY_CONCURRENCY_LIMIT_DEFAULT: usize = Semaphore::MAX_PERMITS;

impl Default for ConfigGrpc {
    fn default() -> Self {
        Self {
            address: SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
                10_000,
            ),
            max_decoding_message_size: MAX_DECODING_MESSAGE_SIZE_DEFAULT,
            channel_capacity: CHANNEL_CAPACITY_DEFAULT,
            unary_concurrency_limit: UNARY_CONCURRENCY_LIMIT_DEFAULT,
            unary_disabled: Default::default(),
            filters: Default::default(),
            normalize_commitment_level: true,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ConfigGrpcFilters {
    pub accounts: ConfigGrpcFiltersAccounts,
    pub slots: ConfigGrpcFiltersSlots,
    pub transactions: ConfigGrpcFiltersTransactions,
    pub blocks: ConfigGrpcFiltersBlocks,
    pub blocks_meta: ConfigGrpcFiltersBlocksMeta,
    pub entry: ConfigGrpcFiltersEntry,
}

impl ConfigGrpcFilters {
    pub fn check_max(len: usize, max: usize) -> anyhow::Result<()> {
        anyhow::ensure!(
            len <= max,
            "Max amount of filters reached, only {} allowed",
            max
        );
        Ok(())
    }

    pub fn check_any(is_empty: bool, any: bool) -> anyhow::Result<()> {
        anyhow::ensure!(
            !is_empty || any,
            "Broadcast `any` is not allowed, at least one filter required"
        );
        Ok(())
    }

    pub fn check_pubkey_max(len: usize, max: usize) -> anyhow::Result<()> {
        anyhow::ensure!(
            len <= max,
            "Max amount of Pubkeys reached, only {} allowed",
            max
        );
        Ok(())
    }

    pub fn check_pubkey_reject(
        pubkey: &Pubkey,
        set: &HashSet<Pubkey>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !set.contains(pubkey),
            "Pubkey {} in filters not allowed",
            pubkey
        );
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ConfigGrpcFiltersAccounts {
    pub max: usize,
    pub any: bool,
    pub account_max: usize,
    pub account_reject: HashSet<Pubkey>,
    pub owner_max: usize,
    pub owner_reject: HashSet<Pubkey>,
}

impl Default for ConfigGrpcFiltersAccounts {
    fn default() -> Self {
        Self {
            max: usize::MAX,
            any: true,
            account_max: usize::MAX,
            account_reject: HashSet::new(),
            owner_max: usize::MAX,
            owner_reject: HashSet::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigGrpcFiltersSlots {
    pub max: usize,
}

impl Default for ConfigGrpcFiltersSlots {
    fn default() -> Self {
        Self { max: usize::MAX }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigGrpcFiltersTransactions {
    pub max: usize,
    pub any: bool,
    pub account_include_max: usize,
    pub account_include_reject: HashSet<Pubkey>,
    pub account_exclude_max: usize,
    pub account_required_max: usize,
}

impl Default for ConfigGrpcFiltersTransactions {
    fn default() -> Self {
        Self {
            max: usize::MAX,
            any: true,
            account_include_max: usize::MAX,
            account_include_reject: HashSet::new(),
            account_exclude_max: usize::MAX,
            account_required_max: usize::MAX,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigGrpcFiltersBlocks {
    pub max: usize,
    pub account_include_max: usize,
    pub account_include_any: bool,
    pub account_include_reject: HashSet<Pubkey>,
    pub include_transactions: bool,
    pub include_accounts: bool,
    pub include_entries: bool,
}

impl Default for ConfigGrpcFiltersBlocks {
    fn default() -> Self {
        Self {
            max: usize::MAX,
            account_include_max: usize::MAX,
            account_include_any: true,
            account_include_reject: HashSet::new(),
            include_transactions: true,
            include_accounts: true,
            include_entries: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigGrpcFiltersBlocksMeta {
    pub max: usize,
}

impl Default for ConfigGrpcFiltersBlocksMeta {
    fn default() -> Self {
        Self { max: usize::MAX }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigGrpcFiltersEntry {
    pub max: usize,
}

impl Default for ConfigGrpcFiltersEntry {
    fn default() -> Self {
        Self { max: usize::MAX }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ConfigBlockFailAction {
    Log,
    Panic,
}

impl Default for ConfigBlockFailAction {
    fn default() -> Self {
        Self::Log
    }
}
