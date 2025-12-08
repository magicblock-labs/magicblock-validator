use solana_pubkey::Pubkey;
use thiserror::Error;

pub type RemoteAccountProviderResult<T> =
    std::result::Result<T, RemoteAccountProviderError>;

#[derive(Debug, Error)]
pub enum RemoteAccountProviderError {
    #[error("Pubsub client error: {0}")]
    PubsubClientError(
        Box<solana_pubsub_client::pubsub_client::PubsubClientError>,
    ),

    #[error(transparent)]
    JoinError(#[from] tokio::task::JoinError),

    #[error(transparent)]
    IndexerError(#[from] light_client::indexer::IndexerError),

    #[error("Receiver error: {0}")]
    RecvrError(#[from] tokio::sync::oneshot::error::RecvError),

    #[error("Account subscription for {0} already exists")]
    AccountSubscriptionAlreadyExists(String),

    #[error("Account subscription for {0} does not exist")]
    AccountSubscriptionDoesNotExist(String),

    #[error("Account subscription receiver already taken")]
    SubscriptionReceiverAlreadyTaken,

    #[error("Failed to send message to pubsub actor: {0} ({1})")]
    ChainPubsubActorSendError(String, String),

    #[error("Failed to manage subscriptions ({0})")]
    AccountSubscriptionsTaskFailed(String),

    #[error("Failed to resolve accounts ({0})")]
    AccountResolutionsFailed(String),

    #[error("Failed to resolve account ({0}) to track slots")]
    ClockAccountCouldNotBeResolved(String),

    #[error("Failed to resolve accounts to same slot ({0}) to track slots")]
    SlotsDidNotMatch(String, Vec<u64>),

    #[error("Accounts matched same slot ({0}), but it's less than min required context slot {2} ")]
    MatchingSlotsNotSatisfyingMinContextSlot(String, Vec<u64>, u64),

    #[error("Failed to fetch accounts ({0})")]
    FailedFetchingAccounts(String),

    #[error("LRU capacity must be greater than 0, got {0}")]
    InvalidLruCapacity(usize),

    #[error(
        "Only one listener supported on lru cache removed accounts events"
    )]
    LruCacheRemoveAccountSenderSupportsSingleReceiverOnly,

    #[error("Failed to send account removal event: {0:?}")]
    FailedToSendAccountRemovalUpdate(
        tokio::sync::mpsc::error::SendError<Pubkey>,
    ),
    #[error("The program account is owned by an unsupported loader: {0}")]
    UnsupportedProgramLoader(String),

    #[error("The LoaderV1 program {0} needs a program account to be provided")]
    LoaderV1StateMissingProgramAccount(Pubkey),

    #[error("The LoaderV2 program {0} needs a program account to be provided")]
    LoaderV2StateMissingProgramAccount(Pubkey),

    #[error(
        "The LoaderV3 program {0} needs a program data account to be provided"
    )]
    LoaderV3StateMissingProgramDataAccount(Pubkey),

    #[error(
        "The LoaderV3 program {0} data account has an invalid length: {1}"
    )]
    LoaderV3StateInvalidLength(Pubkey, usize),

    #[error("The LoaderV4 program {0} needs a program account to be provided")]
    LoaderV4StateMissingProgramAccount(Pubkey),

    #[error("The LoaderV4 program {0} account has an invalid length: {1}")]
    LoaderV4StateInvalidLength(Pubkey, usize),

    #[error("The LoaderV4 program {0} account has invalid program data state")]
    LoaderV4InvalidProgramDataState(Pubkey),
    #[error(
        "The LoaderV4 program {0} account state deserialization failed: {1}"
    )]
    LoaderV4StateDeserializationFailed(Pubkey, String),
}

impl From<solana_pubsub_client::pubsub_client::PubsubClientError>
    for RemoteAccountProviderError
{
    fn from(e: solana_pubsub_client::pubsub_client::PubsubClientError) -> Self {
        Self::PubsubClientError(Box::new(e))
    }
}
