use crate::remote_account_provider::config::RemoteAccountProviderConfig;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LifecycleMode {
    // - clone all accounts
    // - write to all accounts
    Replica,
    // - clone program accounts
    // - write to all accounts
    #[default]
    ProgramsReplica,
    // - clone all accounts
    // - write to delegated accounts
    Ephemeral,
    // - clone no accounts
    // - write to all accounts
    Offline,
}

impl LifecycleMode {
    pub fn is_cloning_all_accounts(&self) -> bool {
        matches!(self, LifecycleMode::Replica | LifecycleMode::Ephemeral)
    }

    pub fn is_cloning_program_accounts(&self) -> bool {
        matches!(self, LifecycleMode::ProgramsReplica)
    }

    pub fn is_watching_accounts(&self) -> bool {
        matches!(self, LifecycleMode::Ephemeral)
    }

    pub fn write_only_delegated_accounts(&self) -> bool {
        matches!(self, LifecycleMode::Ephemeral)
    }

    pub fn can_create_accounts(&self) -> bool {
        !matches!(self, LifecycleMode::Ephemeral)
    }

    pub fn needs_remote_account_provider(&self) -> bool {
        !matches!(self, LifecycleMode::Offline)
    }
}

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
