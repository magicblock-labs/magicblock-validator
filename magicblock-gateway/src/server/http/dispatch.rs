use std::{convert::Infallible, sync::Arc};

use hyper::{body::Incoming, Request, Response};
use magicblock_accounts_db::AccountsDb;
use magicblock_ledger::Ledger;

use crate::{
    error::RpcError,
    requests::{self, payload::ResponseErrorPayload},
    state::transactions::TransactionsCache,
    unwrap,
    utils::JsonBody,
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
        use requests::http::*;
        let response = match request.method {
            GetAccountInfo => {
                get_account_info::handle(request, &self.accountsdb)
            }
            GetMultipleAccounts => {
                get_balance::handle(request, &self.accountsdb)
            }
            GetProgramAccounts => {
                get_program_accounts::handle(request, &self.accountsdb)
            }
            SendTransaction => {
                todo!()
            }
            SimulateTransaction => {
                todo!()
            }
            GetTransaction => get_transaction::handle(request, &self.ledger),
            GetSignatureStatuses => {
                todo!()
            }
            GetSignaturesForAddress => {
                todo!()
            }
            GetTokenAccountsByOwner => {
                get_token_accounts_by_owner::handle(request, &self.accountsdb)
            }
            GetTokenAccountsByDelegate => {
                get_token_accounts_by_delegate::handle(
                    request,
                    &self.accountsdb,
                )
            }
            GetSlot => get_slot::handle(request, &self.accountsdb),
            GetBlock => {
                todo!()
            }
            GetBlocks => get_blocks::handle(request, &self.accountsdb),
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
