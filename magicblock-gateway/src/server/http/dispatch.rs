use std::{convert::Infallible, sync::Arc};

use hyper::{body::Incoming, Request, Response};
use magicblock_accounts_db::AccountsDb;
use magicblock_ledger::Ledger;

use crate::{
    error::RpcError,
    requests::{
        self,
        http::utils::{extract_bytes, parse_body, JsonBody},
        payload::ResponseErrorPayload,
    },
    state::transactions::TransactionsCache,
    unwrap,
};

pub(crate) struct HttpDispatcher {
    pub(crate) accountsdb: Arc<AccountsDb>,
    pub(crate) ledger: Arc<Ledger>,
    pub(crate) transactions: Arc<TransactionsCache>,
}

impl HttpDispatcher {
    pub(crate) async fn dispatch(
        self: Arc<Self>,
        request: Request<Incoming>,
    ) -> Result<Response<JsonBody>, Infallible> {
        let body = unwrap!(extract_bytes(request).await);
        let request = unwrap!(parse_body(body));

        use crate::requests::JsonRpcMethod::*;
        let response = match request.method {
            GetAccountInfo => requests::http::get_account_info::handle(
                request,
                &self.accountsdb,
            ),
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
            unknown => {
                let error = RpcError::method_not_found(unknown);
                return Ok(ResponseErrorPayload::encode(
                    Some(&request.id),
                    error,
                ));
            }
        };
        Ok(response)
    }
}
