use magicblock_config::config::LifecycleMode;

use super::{RemoteAccountProviderError, RemoteAccountProviderResult};

// TODO(thlorenz): make configurable
// Tracked: https://github.com/magicblock-labs/magicblock-validator/issues/577
pub const DEFAULT_SUBSCRIBED_ACCOUNTS_LRU_CAPACITY: usize = 10_000;

#[derive(Debug, Clone)]
pub struct RemoteAccountProviderConfig {
    subscribed_accounts_lru_capacity: usize,
    lifecycle_mode: LifecycleMode,
    enable_subscription_metrics: bool,
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
}

impl Default for RemoteAccountProviderConfig {
    fn default() -> Self {
        Self {
            subscribed_accounts_lru_capacity:
                DEFAULT_SUBSCRIBED_ACCOUNTS_LRU_CAPACITY,
            lifecycle_mode: LifecycleMode::default(),
            enable_subscription_metrics: true,
        }
    }
}
