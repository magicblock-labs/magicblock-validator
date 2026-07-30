use std::io;

use engine::EngineError;
use magicblock_aperture::ApertureError;
use magicblock_chainlink::errors::ChainlinkError;
use magicblock_committor_service::error::CommittorServiceError;
use magicblock_ledger_deprecated::errors::LedgerError;
use magicblock_runtime::Error as RuntimeError;
use magicblock_task_scheduler::TaskSchedulerError;
use replicator::ReplicationError;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::client_error::Error as RpcClientError;
use solana_transaction_error::TransactionError;
use thiserror::Error;

pub type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("IO error: {0}")]
    IoError(#[from] io::Error),

    #[error("Aperture service error: {0}")]
    Aperture(#[from] ApertureError),

    #[error("Ledger error: {0}")]
    LedgerError(Box<LedgerError>),

    #[error("Engine error: {0}")]
    Engine(#[from] EngineError),

    #[error("Replication error: {0}")]
    Replication(#[from] ReplicationError),

    #[error("Runtime image error: {0}")]
    Runtime(#[from] RuntimeError),

    #[error("Chainlink error: {0}")]
    ChainlinkError(Box<ChainlinkError>),

    #[error("Failed to obtain balance for validator '{0}' from chain: {1}")]
    FailedToObtainValidatorOnChainBalance(
        Pubkey,
        #[source] Box<RpcClientError>,
    ),

    #[error(
        "Validator '{0}' is insufficiently funded on chain. Minimum is ({1} SOL)"
    )]
    ValidatorInsufficientlyFunded(Pubkey, u64),

    #[error("Failed to initialize magic fee vault for validator '{0}': {1}")]
    FailedToInitMagicFeeVault(Pubkey, #[source] Box<RpcClientError>),

    #[error("Failed to delegate magic fee vault for validator '{0}': {1}")]
    FailedToDelegateMagicFeeVault(Pubkey, #[source] Box<RpcClientError>),

    #[error("On-chain setup transaction for validator '{0}' was rejected: {1}")]
    OnchainSetupTransactionRejected(Pubkey, #[source] TransactionError),

    #[error("Failed to delegate task scheduler faucet '{0}': {1}")]
    FailedToDelegateFaucet(Pubkey, String),

    #[error("CommittorServiceError")]
    CommittorServiceError(Box<CommittorServiceError>),

    #[error("Unable to clean ledger directory at '{0}'")]
    UnableToCleanLedgerDirectory(String),

    #[error("Failed to start metrics service: {0}")]
    FailedToStartMetricsService(io::Error),

    #[error("Ledger Path is missing a parent directory: {0}")]
    LedgerPathIsMissingParent(String),

    #[error("TaskSchedulerServiceError")]
    TaskSchedulerServiceError(Box<TaskSchedulerError>),

    #[error("Failed to sanitize transaction: {0}")]
    FailedToSanitizeTransaction(#[from] TransactionError),
}

impl From<LedgerError> for ApiError {
    fn from(e: LedgerError) -> Self {
        Self::LedgerError(Box::new(e))
    }
}

impl From<ChainlinkError> for ApiError {
    fn from(e: ChainlinkError) -> Self {
        Self::ChainlinkError(Box::new(e))
    }
}

impl From<CommittorServiceError> for ApiError {
    fn from(e: CommittorServiceError) -> Self {
        Self::CommittorServiceError(Box::new(e))
    }
}

impl From<TaskSchedulerError> for ApiError {
    fn from(e: TaskSchedulerError) -> Self {
        Self::TaskSchedulerServiceError(Box::new(e))
    }
}
