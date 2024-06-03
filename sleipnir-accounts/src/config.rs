use sleipnir_mutator::Cluster;
use solana_sdk::genesis_config::ClusterType;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct AccountsConfig {
    pub external: ExternalConfig,
    pub create: bool,
    pub commit_compute_unit_price: u64,
    pub payer_init_lamports: Option<u64>,
}

// -----------------
// ExternalConfig
// -----------------
#[derive(Debug, PartialEq, Eq)]
pub struct ExternalConfig {
    pub cluster: Cluster,
    pub readonly: ExternalReadonlyMode,
    pub writable: ExternalWritableMode,
}

impl Default for ExternalConfig {
    fn default() -> Self {
        Self {
            cluster: Cluster::Known(ClusterType::Devnet),
            readonly: Default::default(),
            writable: Default::default(),
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub enum ExternalReadonlyMode {
    All,
    #[default]
    Programs,
    None,
}

impl ExternalReadonlyMode {
    pub fn clone_all(&self) -> bool {
        matches!(self, Self::All)
    }
    pub fn clone_programs_only(&self) -> bool {
        matches!(self, Self::Programs)
    }
    pub fn clone_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub enum ExternalWritableMode {
    All,
    Delegated,
    #[default]
    None,
}

impl ExternalWritableMode {
    pub fn clone_all(&self) -> bool {
        matches!(self, Self::All)
    }
    pub fn clone_delegated_only(&self) -> bool {
        matches!(self, Self::Delegated)
    }
    pub fn clone_none(&self) -> bool {
        matches!(self, Self::None)
    }
}
