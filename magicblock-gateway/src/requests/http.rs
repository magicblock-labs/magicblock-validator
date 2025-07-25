use hyper::{body::Incoming, Request, Response};
use utils::{extract_bytes, parse_body, JsonBody};

use crate::{error::RpcError, state::SharedState, RpcResult};

pub(crate) async fn dispatch(
    request: Request<Incoming>,
    state: SharedState,
) -> RpcResult<Response<JsonBody>> {
    let body = extract_bytes(request).await?;
    let request = parse_body(body)?;

    use super::JsonRpcMethod::*;
    match request.method {
        GetAccountInfo => {
            todo!()
        }
        GetMultipleAccounts => {
            todo!()
        }
        GetProgramAccounts => {
            todo!()
        }
        SendTransaction => {
            todo!()
        }
        SimulateTransaction => {
            todo!()
        }
        GetTransaction => {
            todo!()
        }
        GetSignatureStatuses => {
            todo!()
        }
        GetSignaturesForAddress => {
            todo!()
        }
        GetTokenAccountsByOwner => {
            todo!()
        }
        GetTokenAccountsByDelegate => {
            todo!()
        }
        GetSlot => {
            todo!()
        }
        GetBlock => {
            todo!()
        }
        GetBlocks => {
            todo!()
        }
        unknown => Err(RpcError::method_not_found(unknown)),
    }
}

mod get_account_info;
mod get_balance;
mod get_block;
mod get_block_height;
mod get_block_time;
mod get_blocks;
mod get_blocks_with_limit;
mod get_fees_for_message;
mod get_identity;
mod get_latest_blockhash;
mod get_multiple_accounts;
mod get_program_accounts;
mod get_signature_statuses;
mod get_signatures_for_address;
mod get_slot;
mod get_token_account_balance;
mod get_token_accounts_by_delegate;
mod get_token_accounts_by_owner;
mod get_transaction;
mod utils;
