use std::{convert::Infallible, sync::Arc};

use hyper::{body::Incoming, Request, Response};
use magicblock_accounts_db::AccountsDb;
use magicblock_core::link::{
    accounts::EnsureAccountsTx, transactions::TransactionSchedulerHandle,
    DispatchEndpoints,
};
use magicblock_ledger::Ledger;
use solana_pubkey::Pubkey;

use crate::{
    error::RpcError,
    requests::{
        http::{extract_bytes, parse_body},
        payload::ResponseErrorPayload,
    },
    state::{
        blocks::BlocksCache, transactions::TransactionsCache, SharedState,
    },
    utils::JsonBody,
};

/// The central request router for the JSON-RPC HTTP server.
///
/// An instance of `HttpDispatcher` holds all the necessary, thread-safe handles
/// to application state (databases, caches) and communication channels required
/// to process any supported JSON-RPC request. It acts as the `self` context
/// for all RPC method implementations.
pub(crate) struct HttpDispatcher {
    /// The public key of the validator node.
    pub(crate) identity: Pubkey,
    /// A handle to the accounts database.
    pub(crate) accountsdb: Arc<AccountsDb>,
    /// A handle to the blockchain ledger.
    pub(crate) ledger: Arc<Ledger>,
    /// A handle to the transaction signatures cache.
    pub(crate) transactions: TransactionsCache,
    /// A handle to the recent blocks cache.
    pub(crate) blocks: Arc<BlocksCache>,
    /// A sender channel to request that accounts be cloned into ER.
    pub(crate) ensure_accounts_tx: EnsureAccountsTx,
    /// A handle to the transaction scheduler for processing
    /// `sendTransaction` and `simulateTransaction`.
    pub(crate) transactions_scheduler: TransactionSchedulerHandle,
}

impl HttpDispatcher {
    /// Creates a new, thread-safe `HttpDispatcher` instance.
    ///
    /// This constructor clones the necessary handles from the global `SharedState` and
    /// `DispatchEndpoints`, making it cheap to create multiple `Arc<Self>` pointers.
    pub(super) fn new(
        state: &SharedState,
        channels: &DispatchEndpoints,
    ) -> Arc<Self> {
        Arc::new(Self {
            identity: state.identity,
            accountsdb: state.accountsdb.clone(),
            ledger: state.ledger.clone(),
            transactions: state.transactions.clone(),
            blocks: state.blocks.clone(),
            ensure_accounts_tx: channels.ensure_accounts.clone(),
            transactions_scheduler: channels.transaction_scheduler.clone(),
        })
    }

    /// The main entry point for processing a single HTTP request.
    ///
    /// This function orchestrates the entire lifecycle of an RPC request:
    /// 1.  **Parsing**: It extracts and deserializes the raw JSON request body.
    /// 2.  **Routing**: It reads the `method` field and routes the request to the
    ///     appropriate handler function (e.g., `get_account_info`).
    /// 3.  **Execution**: It calls the handler function to process the request.
    /// 4.  **Response**: It serializes the successful result or any error into a
    ///     standard JSON-RPC response.
    ///
    /// This function is designed to never panic or return an `Err`; all errors are
    /// caught and formatted into a valid JSON-RPC error object in the HTTP response.
    pub(super) async fn dispatch(
        self: Arc<Self>,
        request: Request<Incoming>,
    ) -> Result<Response<JsonBody>, Infallible> {
        // A local macro to simplify error handling. If a Result is an Err,
        // it immediately formats it into a JSON-RPC error response and returns.
        macro_rules! unwrap {
            ($result:expr, $id: expr) => {
                match $result {
                    Ok(r) => r,
                    Err(error) => {
                        return Ok(ResponseErrorPayload::encode($id, error));
                    }
                }
            };
        }

        // 1. Extract and parse the request body.
        let body = unwrap!(extract_bytes(request).await, None);
        let mut request = unwrap!(parse_body(body), None);
        let request = &mut request;

        // 2. Route the request to the correct handler based on the method name.
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
            // Handle any methods that are not recognized.
            unknown => Err(RpcError::method_not_found(unknown)),
        };

        // 3. Format the final response, handling any errors from the execution stage.
        Ok(unwrap!(response, Some(&request.id)))
    }
}
