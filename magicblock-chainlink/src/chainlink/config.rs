use crate::{
    remote_account_provider::config::RemoteAccountProviderConfig,
    validator_types::LifecycleMode,
};

#[derive(Debug, Default, Clone)]
pub struct ChainlinkConfig {
    pub remote_account_provider: RemoteAccountProviderConfig,
}

impl ChainlinkConfig {
    pub fn new(remote_account_provider: RemoteAccountProviderConfig) -> Self {
        Self {
            remote_account_provider,
        }
    }

    pub fn default_with_lifecycle_mode(lifecycle_mode: LifecycleMode) -> Self {
        Self {
            remote_account_provider:
                RemoteAccountProviderConfig::default_with_lifecycle_mode(
                    lifecycle_mode,
                ),
        }
    }
}
