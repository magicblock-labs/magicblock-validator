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
