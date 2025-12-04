use magicblock_config::config::LifecycleMode;

use crate::remote_account_provider::config::RemoteAccountProviderConfig;

#[derive(Debug, Default, Clone)]
pub struct ChainlinkConfig {
    pub remote_account_provider: RemoteAccountProviderConfig,
    /// When true, confined accounts are removed during accounts bank reset.
    /// Default: false
    pub remove_confined_accounts: bool,
}

impl ChainlinkConfig {
    pub fn new(remote_account_provider: RemoteAccountProviderConfig) -> Self {
        Self {
            remote_account_provider,
            remove_confined_accounts: false,
        }
    }

    pub fn default_with_lifecycle_mode(lifecycle_mode: LifecycleMode) -> Self {
        Self {
            remote_account_provider:
                RemoteAccountProviderConfig::default_with_lifecycle_mode(
                    lifecycle_mode,
                ),
            remove_confined_accounts: false,
        }
    }
}
