use conjunto_transwise::AccountChainSnapshotShared;
use solana_sdk::pubkey::Pubkey;
use tokio::sync::oneshot::Sender;

pub struct RemoteAccountFetcherRequest {
    account: Pubkey,
}

#[derive(Debug)]
pub enum RemoteAccountFetcherResponse {
    InFlight {
        listeners: Vec<Sender<RemoteAccountFetcherResult>>,
    },
    Available {
        result: RemoteAccountFetcherResult,
    },
}

// The result type must be clonable (we use a string as error)
pub type RemoteAccountFetcherResult =
    Result<AccountChainSnapshotShared, String>;
