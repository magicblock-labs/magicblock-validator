use std::{collections::HashSet, time::Duration};

use magicblock_config::{
    config::{GrpcConfig, LifecycleMode},
    consts::DEFAULT_RESUBSCRIPTION_DELAY_MS,
};
use solana_pubkey::Pubkey;

use super::{RemoteAccountProviderError, RemoteAccountProviderResult};

#[derive(Debug, Clone)]
pub struct RemoteAccountProviderConfig {
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
    /// Global gRPC configuration
    grpc: GrpcConfig,
}

impl RemoteAccountProviderConfig {
    pub fn try_new(
        lifecycle_mode: LifecycleMode,
    ) -> RemoteAccountProviderResult<Self> {
        Self::try_new_with_metrics(lifecycle_mode, true)
    }

    pub fn try_new_with_metrics(
        lifecycle_mode: LifecycleMode,
        enable_subscription_metrics: bool,
    ) -> RemoteAccountProviderResult<Self> {
        Ok(Self {
            lifecycle_mode,
            enable_subscription_metrics,
            resubscription_delay: std::time::Duration::from_millis(
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

    pub fn lifecycle_mode(&self) -> &LifecycleMode {
        &self.lifecycle_mode
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

    pub fn grpc(&self) -> &GrpcConfig {
        &self.grpc
    }

    pub fn with_grpc(mut self, grpc: GrpcConfig) -> Self {
        self.grpc = grpc;
        self
    }
}

impl Default for RemoteAccountProviderConfig {
    fn default() -> Self {
        Self {
            lifecycle_mode: LifecycleMode::default(),
            enable_subscription_metrics: true,
            program_subs: vec![dlp_api::id()].into_iter().collect(),
            resubscription_delay: std::time::Duration::from_millis(
                DEFAULT_RESUBSCRIPTION_DELAY_MS,
            ),
            grpc: GrpcConfig::default(),
        }
    }
}
