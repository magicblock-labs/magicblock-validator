use std::{convert::Infallible, sync::Arc};

use hyper::{body::Incoming, Request, Response};
use magicblock_accounts_db::AccountsDb;
use magicblock_gateway_types::{
    accounts::{EnsureAccountsTx, Pubkey},
    transactions::TxnExecutionTx,
    RpcChannelEndpoints,
};
use magicblock_ledger::Ledger;

use crate::{
    error::RpcError,
    requests::{
        http::{extract_bytes, parse_body},
        payload::ResponseErrorPayload,
    },
    state::{
        blocks::BlocksCache, transactions::TransactionsCache, SharedState,
    },
    unwrap,
    utils::JsonBody,
};

pub(crate) struct HttpDispatcher {
    pub(crate) identity: Pubkey,
    pub(crate) accountsdb: Arc<AccountsDb>,
    pub(crate) ledger: Arc<Ledger>,
    pub(crate) transactions: TransactionsCache,
    pub(crate) blocks: Arc<BlocksCache>,
    pub(crate) ensure_accounts_tx: EnsureAccountsTx,
    pub(crate) transaction_execution_tx: TxnExecutionTx,
}

impl HttpDispatcher {
    pub(super) fn new(
        state: &SharedState,
        channels: &RpcChannelEndpoints,
    ) -> Arc<Self> {
        Self {
            identity: state.identity,
            accountsdb: state.accountsdb.clone(),
            ledger: state.ledger.clone(),
            transactions: state.transactions.clone(),
            blocks: state.blocks.clone(),
            ensure_accounts_tx: channels.ensure_accounts_tx.clone(),
            transaction_execution_tx: channels.transaction_execution_tx.clone(),
        }
        .into()
    }

    pub(super) async fn dispatch(
        self: Arc<Self>,
        request: Request<Incoming>,
    ) -> Result<Response<JsonBody>, Infallible> {
        let body = unwrap!(extract_bytes(request).await);
        let request = unwrap!(parse_body(body));

        use crate::requests::JsonRpcMethod::*;
        let response = match request.method {
            GetAccountInfo => self.get_account_info(request).await,
            GetBalance => self.get_balance(request).await,
            GetMultipleAccounts => self.get_multiple_accounts(request).await,
            GetProgramAccounts => self.get_program_accounts(request),
            SendTransaction => self.send_transaction(request).await,
            SimulateTransaction => self.simulate_transaction(request).await,
            GetTransaction => self.get_transaction(request),
            GetSignatureStatuses => self.get_signature_statuses(request),
            GetSignaturesForAddress => self.get_signatures_for_address(request),
            GetTokenAccountBalance => {
                self.get_token_account_balance(request).await
            }
            GetTokenAccountsByOwner => {
                self.get_token_accounts_by_owner(request)
            }
            GetTokenAccountsByDelegate => {
                self.get_token_accounts_by_delegate(request)
            }
            GetSlot => self.get_slot(request),
            GetBlock => self.get_block(request),
            GetBlocks => self.get_blocks(request),
            GetBlocksWithLimit => self.get_blocks_with_limit(request),
            GetLatestBlockhash => self.get_latest_blockhash(request),
            GetBlockHeight => self.get_block_height(request),
            GetIdentity => self.get_identity(request),
            IsBlockhashValid => self.is_blockhash_valid(request),
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
