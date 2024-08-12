use solana_sdk::pubkey::Pubkey;

#[derive(Debug)]
pub struct RemoteAccountUpdatesRequest {
    pub account: Pubkey,
}
