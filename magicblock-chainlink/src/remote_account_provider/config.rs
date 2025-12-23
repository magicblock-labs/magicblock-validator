use std::collections::HashSet;

use magicblock_config::config::LifecycleMode;
use solana_pubkey::Pubkey;

use super::{RemoteAccountProviderError, RemoteAccountProviderResult};

// TODO(thlorenz): make configurable
// Tracked: https://github.com/magicblock-labs/magicblock-validator/issues/577
pub const DEFAULT_SUBSCRIBED_ACCOUNTS_LRU_CAPACITY: usize = 10_000;

#[derive(Debug, Clone)]
pub struct RemoteAccountProviderConfig {
    /// How many accounts to monitor for changes
    subscribed_accounts_lru_capacity: usize,
    /// Lifecycle mode of the validator
    lifecycle_mode: LifecycleMode,
    /// Whether to enable metrics for account subscriptions
    enable_subscription_metrics: bool,
    /// Set of program accounts to always subscribe to as backup
    /// for direct account subs
    program_subs: HashSet<Pubkey>,
}

impl RemoteAccountProviderConfig {
    pub fn try_new(
        subscribed_accounts_lru_capacity: usize,
        lifecycle_mode: LifecycleMode,
    ) -> RemoteAccountProviderResult<Self> {
        Self::try_new_with_metrics(
            subscribed_accounts_lru_capacity,
            lifecycle_mode,
            true,
        )
    }

    pub fn try_new_with_metrics(
        subscribed_accounts_lru_capacity: usize,
        lifecycle_mode: LifecycleMode,
        enable_subscription_metrics: bool,
    ) -> RemoteAccountProviderResult<Self> {
        if subscribed_accounts_lru_capacity == 0 {
            return Err(RemoteAccountProviderError::InvalidLruCapacity(
                subscribed_accounts_lru_capacity,
            ));
        }
        Ok(Self {
            subscribed_accounts_lru_capacity,
            lifecycle_mode,
            enable_subscription_metrics,
            ..Default::default()
        })
    }

    pub fn default_with_lifecycle_mode(lifecycle_mode: LifecycleMode) -> Self {
        Self {
            lifecycle_mode,
            ..Default::default()
        }
    }

    pub fn lifecycle_mode(&self) -> &LifecycleMode {
        &self.lifecycle_mode
    }

    pub fn subscribed_accounts_lru_capacity(&self) -> usize {
        self.subscribed_accounts_lru_capacity
    }

    pub fn enable_subscription_metrics(&self) -> bool {
        self.enable_subscription_metrics
    }

    pub fn program_subs(&self) -> &HashSet<Pubkey> {
        &self.program_subs
    }
}

impl Default for RemoteAccountProviderConfig {
    fn default() -> Self {
        Self {
            subscribed_accounts_lru_capacity:
                DEFAULT_SUBSCRIBED_ACCOUNTS_LRU_CAPACITY,
            lifecycle_mode: LifecycleMode::default(),
            enable_subscription_metrics: true,
            program_subs: vec![dlp::id()].into_iter().collect(),
        }
    }
}
