use std::{collections::HashSet, time::Duration};

use magicblock_config::{
    config::LifecycleMode,
    consts::{DEFAULT_MAX_MONITORED_ACCOUNTS, DEFAULT_RESUBSCRIPTION_DELAY_MS},
};
use solana_pubkey::Pubkey;

use super::{RemoteAccountProviderError, RemoteAccountProviderResult};

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
    /// Delay between resubscribing to accounts after a pubsub
    /// reconnection
    resubscription_delay: Duration,
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
            return Err(RemoteAccountProviderError::InvalidLruCapacity);
        }
        Ok(Self {
            subscribed_accounts_lru_capacity,
            lifecycle_mode,
            enable_subscription_metrics,
            resubscription_delay: Duration::from_millis(
                DEFAULT_RESUBSCRIPTION_DELAY_MS,
            ),
            ..Default::default()
        })
    }

    pub fn default_with_lifecycle_mode(lifecycle_mode: LifecycleMode) -> Self {
        Self {
            lifecycle_mode,
            ..Default::default()
        }
    }

    pub fn with_resubscription_delay(
        mut self,
        delay: Duration,
    ) -> RemoteAccountProviderResult<Self> {
        if delay == Duration::ZERO {
            return Err(RemoteAccountProviderError::InvalidResubscriptionDelay);
        }
        self.resubscription_delay = delay;
        Ok(self)
    }

    pub fn with_subscribed_accounts_lru_capacity(
        mut self,
        capacity: usize,
    ) -> RemoteAccountProviderResult<Self> {
        if capacity == 0 {
            return Err(RemoteAccountProviderError::InvalidLruCapacity);
        }
        self.subscribed_accounts_lru_capacity = capacity;
        Ok(self)
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

    pub fn resubscription_delay(&self) -> Duration {
        self.resubscription_delay
    }
}

impl Default for RemoteAccountProviderConfig {
    fn default() -> Self {
        Self {
            subscribed_accounts_lru_capacity: DEFAULT_MAX_MONITORED_ACCOUNTS,
            lifecycle_mode: LifecycleMode::default(),
            enable_subscription_metrics: true,
            program_subs: vec![dlp::id()].into_iter().collect(),
            resubscription_delay: Duration::from_millis(
                DEFAULT_RESUBSCRIPTION_DELAY_MS,
            ),
        }
    }
}
