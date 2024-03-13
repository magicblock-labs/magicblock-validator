use jsonrpc_core::{Error, Result};
use solana_sdk::pubkey::Pubkey;

pub fn verify_pubkey(input: &str) -> Result<Pubkey> {
    input
        .parse()
        .map_err(|e| Error::invalid_params(format!("Invalid param: {e:?}")))
}
