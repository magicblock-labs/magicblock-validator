//! Shared account decoding helpers for optional `fetch` integration.

#[cfg(feature = "fetch")]
#[derive(Debug, Clone)]
pub struct DecodedAccount<T> {
    pub address: solana_pubkey::Pubkey,
    pub account: solana_account::Account,
    pub data: T,
}

#[cfg(feature = "fetch")]
#[derive(Debug, Clone)]
pub enum MaybeAccount<T> {
    Exists(DecodedAccount<T>),
    NotFound(solana_pubkey::Pubkey),
}
