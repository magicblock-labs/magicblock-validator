use magicblock_accounts_db::error::AccountsDbError;
use solana_sdk::pubkey::Pubkey;
use thiserror::Error;

pub type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("RPC service error: {0}")]
    RpcError(#[from] magicblock_aperture::error::RpcError),

    #[error("Accounts error: {0}")]
    AccountsError(Box<magicblock_accounts::errors::AccountsError>),

    #[error("AccountCloner error: {0}")]
    AccountClonerError(Box<magicblock_account_cloner::AccountClonerError>),

    #[error("Ledger error: {0}")]
    LedgerError(Box<magicblock_ledger::errors::LedgerError>),

    #[error("Chainlink error: {0}")]
    ChainlinkError(Box<magicblock_chainlink::errors::ChainlinkError>),

    #[error("Failed to obtain balance for validator '{0}' from chain. ({1})")]
    FailedToObtainValidatorOnChainBalance(Pubkey, String),

    #[error("Validator '{0}' is insufficiently funded on chain. Minimum is ({1} SOL)")]
    ValidatorInsufficientlyFunded(Pubkey, u64),

    #[error("CommittorServiceError")]
    CommittorServiceError(
        Box<magicblock_committor_service::error::CommittorServiceError>,
    ),

    #[error("Failed to load programs into bank: {0}")]
    FailedToLoadProgramsIntoBank(String),

    #[error("Failed to initialize JSON RPC service: {0}")]
    FailedToInitJsonRpcService(String),

    #[error("Failed to start JSON RPC service: {0}")]
    FailedToStartJsonRpcService(String),

    #[error("Failed to register validator on chain: {0}")]
    FailedToRegisterValidatorOnChain(String),

    #[error("Failed to unregister validator on chain: {0}")]
    FailedToUnregisterValidatorOnChain(String),

    #[error("Unable to clean ledger directory at '{0}'")]
    UnableToCleanLedgerDirectory(String),

    #[error("Failed to start metrics service: {0}")]
    FailedToStartMetricsService(std::io::Error),

    #[error("Ledger Path is missing a parent directory: {0}")]
    LedgerPathIsMissingParent(String),

    #[error("Ledger Path has an invalid faucet keypair file: {0} ({1})")]
    LedgerInvalidFaucetKeypair(String, String),

    #[error("Ledger Path is missing a faucet keypair file: {0}")]
    LedgerIsMissingFaucetKeypair(String),

    #[error("Ledger could not write faucet keypair file: {0} ({1})")]
    LedgerCouldNotWriteFaucetKeypair(String, String),

    #[error("Ledger Path has an invalid validator keypair file: {0} ({1})")]
    LedgerInvalidValidatorKeypair(String, String),

    #[error("Ledger Path is missing a validator keypair file: {0}")]
    LedgerIsMissingValidatorKeypair(String),

    #[error("Ledger could not write validator keypair file: {0} ({1})")]
    LedgerCouldNotWriteValidatorKeypair(String, String),

    #[error(
        "Ledger validator keypair '{0}' needs to match the provided one '{1}'"
    )]
    LedgerValidatorKeypairNotMatchingProvidedKeypair(String, String),

    #[error("The slot at which we should continue after processing the ledger ({0}) does not match the bank slot ({1})"
    )]
    NextSlotAfterLedgerProcessingNotMatchingBankSlot(u64, u64),

    #[error("Accounts Database couldn't be initialized")]
    AccountsDbError(#[from] AccountsDbError),

    #[error("TaskSchedulerServiceError")]
    TaskSchedulerServiceError(
        Box<magicblock_task_scheduler::TaskSchedulerError>,
    ),

    #[error("Failed to sanitize transaction: {0}")]
    FailedToSanitizeTransaction(
        #[from] solana_sdk::transaction::TransactionError,
    ),
}

impl From<magicblock_accounts::errors::AccountsError> for ApiError {
    fn from(e: magicblock_accounts::errors::AccountsError) -> Self {
        Self::AccountsError(Box::new(e))
    }
}

impl From<magicblock_account_cloner::AccountClonerError> for ApiError {
    fn from(e: magicblock_account_cloner::AccountClonerError) -> Self {
        Self::AccountClonerError(Box::new(e))
    }
}

impl From<magicblock_ledger::errors::LedgerError> for ApiError {
    fn from(e: magicblock_ledger::errors::LedgerError) -> Self {
        Self::LedgerError(Box::new(e))
    }
}

impl From<magicblock_chainlink::errors::ChainlinkError> for ApiError {
    fn from(e: magicblock_chainlink::errors::ChainlinkError) -> Self {
        Self::ChainlinkError(Box::new(e))
    }
}

impl From<magicblock_committor_service::error::CommittorServiceError> for ApiError {
    fn from(e: magicblock_committor_service::error::CommittorServiceError) -> Self {
        Self::CommittorServiceError(Box::new(e))
    }
}

impl From<magicblock_task_scheduler::TaskSchedulerError> for ApiError {
    fn from(e: magicblock_task_scheduler::TaskSchedulerError) -> Self {
        Self::TaskSchedulerServiceError(Box::new(e))
    }
}
