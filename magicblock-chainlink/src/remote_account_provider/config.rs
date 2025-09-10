use crate::config::LifecycleMode;

use super::{RemoteAccountProviderError, RemoteAccountProviderResult};

pub const DEFAULT_SUBSCRIBED_ACCOUNTS_LRU_CAPACITY: usize = 1_0000;

#[derive(Debug, Clone)]
pub struct RemoteAccountProviderConfig {
    subscribed_accounts_lru_capacity: usize,
    lifecycle_mode: LifecycleMode,
}

impl RemoteAccountProviderConfig {
    pub fn try_new(
        subscribed_accounts_lru_capacity: usize,
        lifecycle_mode: LifecycleMode,
    ) -> RemoteAccountProviderResult<Self> {
        if subscribed_accounts_lru_capacity == 0 {
            return Err(RemoteAccountProviderError::InvalidLruCapacity(
                subscribed_accounts_lru_capacity,
            ));
        }
        Ok(Self {
            subscribed_accounts_lru_capacity,
            lifecycle_mode,
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
}

impl Default for RemoteAccountProviderConfig {
    fn default() -> Self {
        Self {
            subscribed_accounts_lru_capacity:
                DEFAULT_SUBSCRIBED_ACCOUNTS_LRU_CAPACITY,
            lifecycle_mode: LifecycleMode::default(),
        }
    }
}
