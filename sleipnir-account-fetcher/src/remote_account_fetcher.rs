use solana_sdk::pubkey::Pubkey;
use tokio::sync::oneshot::Sender;

use crate::AccountFetcherResult;

pub struct RemoteAccountFetcherRequest {
    pub account: Pubkey,
}

#[derive(Debug)]
pub enum RemoteAccountFetcherResponse {
    InFlight {
        listeners: Vec<Sender<AccountFetcherResult>>,
    },
    Available {
        result: AccountFetcherResult,
    },
}
