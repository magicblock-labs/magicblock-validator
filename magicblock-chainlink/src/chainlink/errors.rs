use std::sync::Arc;

use magicblock_aml::RiskError;
use solana_program::program_error::ProgramError;
use solana_pubkey::Pubkey;
use thiserror::Error;

use crate::{
    cloner::DelegationIdentity,
    remote_account_provider::RemoteAccountProviderError,
};

pub type ChainlinkResult<T> = std::result::Result<T, ChainlinkError>;

#[derive(Debug, Error)]
pub enum ChainlinkError {
    #[error("Remote account provider error: {0}")]
    RemoteAccountProviderError(
        #[from] crate::remote_account_provider::RemoteAccountProviderError,
    ),
    #[error("JoinError: {0}")]
    JoinError(#[from] tokio::task::JoinError),

    #[error("Cloner error: {0}")]
    ClonerError(#[from] crate::cloner::errors::ClonerError),

    #[error("Timed out ensuring accounts after {0}s")]
    EnsureAccountsTimeout(u64),

    #[error("Delegation record could not be decoded: {0} ({1:?})")]
    InvalidDelegationRecord(Pubkey, ProgramError),

    #[error("Delegation actions could not be decoded: {0} ({1})")]
    InvalidDelegationActions(Pubkey, String),

    #[error("Invalid delegation deduplication configuration: {0}")]
    InvalidDelegationDedupConfig(&'static str),

    #[error("Delegation admission capacity exhausted ({0})")]
    DelegationAdmissionCapacity(&'static str),

    #[error("Delegation admission is closed during shutdown")]
    DelegationAdmissionClosed,

    #[error("Delegated clone target {0} has no delegation identity")]
    MissingDelegationIdentity(Pubkey),

    #[error("Delegation {identity:?} was already processed; clone target {clone_target} is unavailable")]
    DelegationAlreadyProcessed {
        identity: DelegationIdentity,
        clone_target: Pubkey,
    },

    #[error("Delegation {identity:?} activation failed: {source}")]
    DelegationActivationFailed {
        identity: DelegationIdentity,
        source: Arc<ChainlinkError>,
    },

    #[error("Delegation {0:?} activation owner terminated without a result")]
    DelegationActivationAbandoned(DelegationIdentity),

    #[error("Token account could not be decoded while cloning: {0} ({1})")]
    InvalidTokenAccount(Pubkey, String),

    #[error("Failed to resolve one or more accounts {0} when getting delegation records")]
    DelegatedAccountResolutionsFailed(String),

    #[error("Failed to find account that was just resolved {0}")]
    ResolvedAccountCouldNoLongerBeFound(Pubkey),

    #[error("Failed to find companion account that was just resolved {0}")]
    ResolvedCompanionAccountCouldNoLongerBeFound(Pubkey),

    #[error("Failed to subscribe to account {0}: {1:?}")]
    FailedToSubscribeToAccount(Pubkey, RemoteAccountProviderError),

    #[error("Failed to resolve program data account {0} for program {1}")]
    FailedToResolveProgramDataAccount(Pubkey, Pubkey),

    #[error("Failed to resolve/deserialize one or more accounts {0} when getting programs")]
    ProgramAccountResolutionsFailed(String),

    #[error("Unexpected number of accounts returned when fetching account with companion: {0}")]
    UnexpectedAccountCount(String),

    #[error("Missing accounts required by delegation actions: {0:?}")]
    MissingDelegationActionAccounts(Vec<Pubkey>),

    #[error("timeout waiting for pending request for {0}")]
    PendingRequestTimeout(Pubkey),

    #[error("pending request cancelled for {0}")]
    PendingRequestCancelled(Pubkey),

    #[error("pending request owner disappeared for {0}: {1}")]
    PendingRequestOwnerDisappeared(Pubkey, String),

    #[error("missing pending request owner for {0}")]
    MissingPendingRequestOwner(Pubkey),

    #[error("pending request owner failed for {0}: {1}")]
    PendingRequestOwnerFailed(Pubkey, String),

    #[error("Failed to perform Range risk check: {0}")]
    RangeRisk(#[from] RiskError),

    #[error("Chainlink is disabled for non-primary mode")]
    DisabledForNonPrimaryMode,
}
