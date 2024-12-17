use magicblock_core::magic_program;
use solana_rpc_client_api::client_error;
use solana_sdk::pubkey::Pubkey;

pub fn pubkey_from_magic_program(pubkey: magic_program::Pubkey) -> Pubkey {
    Pubkey::from(pubkey.to_bytes())
}

pub fn get_rpc_transwise_error_msg(err: &anyhow::Error) -> Option<String> {
    err.source()
        .and_then(|err| err.downcast_ref::<client_error::Error>())
        .and_then(|err| match err.kind() {
            client_error::ErrorKind::RpcError(err) => {
                use solana_rpc_client_api::request::RpcError::*;
                match err {
                    RpcResponseError { message, .. } => Some(message.clone()),
                    _ => None,
                }
            }
            _ => None,
        })
}
