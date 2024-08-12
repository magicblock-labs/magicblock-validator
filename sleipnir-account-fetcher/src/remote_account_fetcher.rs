use solana_sdk::pubkey::Pubkey;

#[derive(Debug)]
pub struct RemoteAccountFetcherRequest {
    pub account: Pubkey,
}
