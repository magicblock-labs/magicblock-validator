// Adapted from yellowstone-grpc/yellowstone-grpc-geyser/src/config.rs
use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use solana_sdk::pubkey::Pubkey;
use tokio::sync::Semaphore;

#[derive(Debug, Clone)]
pub struct Config {
    pub grpc: ConfigGrpc,
    /// Action on block re-construction error
    pub block_fail_action: ConfigBlockFailAction,
    /// TTL of cached transaction messages
    /// Only applies if [Config::cache_transactions] is `true`
    pub transactions_cache_ttl: Duration,
    /// number of 4-bit access counters to keep for admission and eviction
    /// Only applies if [Config::cache_transactions] is `true`
    pub transactions_cache_num_counters: usize,
    /// Since we ignore internal_cost, in our case this is exactly the same as
    /// transactions_cache_max_cost which affects how eviction decisions are made
    /// If max_cost is 100 and a new item with a cost of 1 increases total cache cost to
    /// 101, 1 item will be evicted
    /// Since all our items are considered to have the same cost what actually happens is
    /// that the item is not added to the cache.
    /// Thus we need to make sure this is higher than we ever expect the cache to grow to
    /// since we cannot miss transaction signatures.
    /// Diagnose this cache and related settings by setting the `DIAG_GEYSER_TX_CACHE_INTERVAL`
    /// compile time environment var.
    /// Only applies if [Config::cache_transactions] is `true`
    pub transactions_cache_max_cached_items: i64,

    /// TTL of cached account messages
    /// Only applies if [Config::cache_accounts] is `true`
    pub accounts_cache_ttl: Duration,
    /// See [Config::transactions_cache_num_counters].
    /// Only applies if [Config::cache_accounts] is `true`
    pub accounts_cache_num_counters: usize,
    /// See [Config::transactions_max_cached_items].
    /// By default it is set to 1GB
    /// When we add an account we take its data size into account when determining
    /// cost, such that large accounts would be evicted first.
    /// Thus if this is set to 100 bytes it can hold 100 empty accounts or 20 accounts with
    /// data byte size of 5 each.
    /// Devs usually subscribe to updates of an account up front and then run lots
    /// of transactions. Therefore it's not that big of a problem to miss the first one in most
    /// cases in case the cache was full and it wasn't added.
    /// Another important aspect is that if we get an update for an account that is already
    /// in the cache it will be replaced with the new data and thus doesn't grow the cache.
    /// Diagnose this cache and related settings by setting the `DIAG_GEYSER_ACC_CACHE_INTERVAL`
    /// compile time environment var.
    pub accounts_cache_max_cached_bytes: i64,

    /// If to cache account updates (default: true)
    pub cache_accounts: bool,
    /// If to cache transaction updates (default: true)
    pub cache_transactions: bool,

    /// If we should register to receive account notifications, (default: true)
    pub enable_account_notifications: bool,
    /// If we should register to receive tranaction notifications, (default: true)
    pub enable_transaction_notifications: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            grpc: Default::default(),
            block_fail_action: Default::default(),
            transactions_cache_ttl: Duration::from_millis(500),
            // Dgraph's developers have seen good performance in setting this to 10x the number of
            // items you expect to keep in the cache when full
            transactions_cache_num_counters: 10_000,
            transactions_cache_max_cached_items: 1_000_000,

            accounts_cache_ttl: Duration::from_millis(500),
            accounts_cache_num_counters: 10_000,
            accounts_cache_max_cached_bytes: 1024 * 1024 * 1024, // 1GB

            cache_accounts: true,
            cache_transactions: true,

            enable_account_notifications: true,
            enable_transaction_notifications: true,
        }
    }
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
            filters: ConfigGrpcFilters {
                transactions: ConfigGrpcFiltersTransactions {
                    any: false,
                    ..Default::default()
                },
                ..Default::default()
            },
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
