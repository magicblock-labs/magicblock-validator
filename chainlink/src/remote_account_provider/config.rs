use std::{collections::HashSet, time::Duration};

use magicblock_config::{
    config::GrpcConfig, consts::DEFAULT_RESUBSCRIPTION_DELAY_MS,
};
use solana_pubkey::Pubkey;

use super::{RemoteAccountProviderError, RemoteAccountProviderResult};

#[derive(Debug, Clone)]
pub struct RemoteAccountProviderConfig {
    /// Whether to enable metrics for account subscriptions
    enable_subscription_metrics: bool,
    /// Set of program accounts to always subscribe to as backup
    /// for direct account subs
    program_subs: HashSet<Pubkey>,
    /// Delay between resubscribing to accounts after a pubsub
    /// reconnection
    resubscription_delay: Duration,
    /// Max subscriptions per websocket connection; overrides the
    /// per-provider defaults when set
    ws_subs_per_connection: Option<usize>,
    /// Global gRPC configuration
    grpc: GrpcConfig,
}

impl RemoteAccountProviderConfig {
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

    pub fn with_subscription_metrics(mut self, enabled: bool) -> Self {
        self.enable_subscription_metrics = enabled;
        self
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

    pub fn with_ws_subs_per_connection(
        mut self,
        limit: Option<usize>,
    ) -> RemoteAccountProviderResult<Self> {
        if limit == Some(0) {
            return Err(RemoteAccountProviderError::InvalidWsSubsPerConnection);
        }
        self.ws_subs_per_connection = limit;
        Ok(self)
    }

    pub fn ws_subs_per_connection(&self) -> Option<usize> {
        self.ws_subs_per_connection
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
            enable_subscription_metrics: true,
            program_subs: HashSet::from([dlp_api::id()]),
            resubscription_delay: std::time::Duration::from_millis(
                DEFAULT_RESUBSCRIPTION_DELAY_MS,
            ),
            ws_subs_per_connection: None,
            grpc: GrpcConfig::default(),
        }
    }
}
