use magicblock_committor_service::service::IntentExecutionServiceError;
use solana_pubkey::Pubkey;
use thiserror::Error;

pub type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Aperture service error: {0}")]
    Aperture(#[from] magicblock_aperture::ApertureError),

    #[error("Ledger error: {0}")]
    LedgerError(Box<magicblock_ledger_deprecated::errors::LedgerError>),

    #[error("Engine error: {0}")]
    Engine(#[from] engine::EngineError),

    #[error("Replication error: {0}")]
    Replication(#[from] replicator::ReplicationError),

    #[error("Runtime image error: {0}")]
    Runtime(#[from] magicblock_runtime::Error),

    #[error("Chainlink error: {0}")]
    ChainlinkError(Box<magicblock_chainlink::errors::ChainlinkError>),

    #[error("Failed to obtain balance for validator '{0}' from chain. ({1})")]
    FailedToObtainValidatorOnChainBalance(Pubkey, String),

    #[error(
        "Validator '{0}' is insufficiently funded on chain. Minimum is ({1} SOL)"
    )]
    ValidatorInsufficientlyFunded(Pubkey, u64),

    #[error("Failed to initialize magic fee vault for validator '{0}': {1}")]
    FailedToInitMagicFeeVault(Pubkey, String),

    #[error("Failed to delegate magic fee vault for validator '{0}': {1}")]
    FailedToDelegateMagicFeeVault(Pubkey, String),

    #[error("On-chain setup transaction for validator '{0}' was rejected: {1}")]
    OnchainSetupTransactionRejected(Pubkey, String),

    #[error("CommittorServiceError")]
    CommittorServiceError(
        Box<magicblock_committor_service::error::CommittorServiceError>,
    ),

    #[error("IntentExecutionServiceError: {0}")]
    IntentExecutionServiceError(#[from] IntentExecutionServiceError),

    #[error("Unable to clean ledger directory at '{0}'")]
    UnableToCleanLedgerDirectory(String),

    #[error("Failed to start metrics service: {0}")]
    FailedToStartMetricsService(std::io::Error),

    #[error("Ledger Path is missing a parent directory: {0}")]
    LedgerPathIsMissingParent(String),

    #[error("TaskSchedulerServiceError")]
    TaskSchedulerServiceError(
        Box<magicblock_task_scheduler::TaskSchedulerError>,
    ),

    #[error("Failed to sanitize transaction: {0}")]
    FailedToSanitizeTransaction(
        #[from] solana_transaction_error::TransactionError,
    ),
}

impl From<magicblock_ledger_deprecated::errors::LedgerError> for ApiError {
    fn from(e: magicblock_ledger_deprecated::errors::LedgerError) -> Self {
        Self::LedgerError(Box::new(e))
    }
}

impl From<magicblock_chainlink::errors::ChainlinkError> for ApiError {
    fn from(e: magicblock_chainlink::errors::ChainlinkError) -> Self {
        Self::ChainlinkError(Box::new(e))
    }
}

impl From<magicblock_committor_service::error::CommittorServiceError>
    for ApiError
{
    fn from(
        e: magicblock_committor_service::error::CommittorServiceError,
    ) -> Self {
        Self::CommittorServiceError(Box::new(e))
    }
}

impl From<magicblock_task_scheduler::TaskSchedulerError> for ApiError {
    fn from(e: magicblock_task_scheduler::TaskSchedulerError) -> Self {
        Self::TaskSchedulerServiceError(Box::new(e))
    }
}
