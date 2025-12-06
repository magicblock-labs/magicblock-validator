use core::str;
use std::{convert::Infallible, sync::Arc};

use futures::{stream::FuturesOrdered, StreamExt};
use hyper::{
    body::Incoming,
    header::{
        HeaderValue, ACCESS_CONTROL_ALLOW_HEADERS,
        ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
        ACCESS_CONTROL_MAX_AGE,
    },
    Method, Request, Response,
};
use magicblock_accounts_db::AccountsDb;
use magicblock_core::link::{
    transactions::TransactionSchedulerHandle, DispatchEndpoints,
};
use magicblock_ledger::Ledger;
use magicblock_metrics::metrics::{
    RPC_REQUESTS_COUNT, RPC_REQUEST_HANDLING_TIME,
};

use crate::{
    requests::{
        http::{extract_bytes, parse_body, HandlerResult},
        payload::ResponseErrorPayload,
        JsonHttpRequest, RpcRequest,
    },
    state::{
        blocks::BlocksCache, transactions::TransactionsCache, ChainlinkImpl,
        NodeContext, SharedState,
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
    pub(crate) context: NodeContext,
    /// A handle to the accounts database.
    pub(crate) accountsdb: Arc<AccountsDb>,
    /// A handle to the blockchain ledger.
    pub(crate) ledger: Arc<Ledger>,
    /// Chainlink provides synchronization of on-chain accounts and
    /// fetches accounts used in a specific transaction as well as those
    /// required when getting account info, etc.
    pub(crate) chainlink: Arc<ChainlinkImpl>,
    /// A handle to the transaction signatures cache.
    pub(crate) transactions: TransactionsCache,
    /// A handle to the recent blocks cache.
    pub(crate) blocks: Arc<BlocksCache>,
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
        state: SharedState,
        channels: &DispatchEndpoints,
    ) -> Arc<Self> {
        Arc::new(Self {
            context: state.context,
            accountsdb: state.accountsdb.clone(),
            ledger: state.ledger.clone(),
            chainlink: state.chainlink,
            transactions: state.transactions.clone(),
            blocks: state.blocks.clone(),
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
        if request.method() == Method::OPTIONS {
            let mut response = Response::new(JsonBody::from(""));
            Self::set_access_control_headers(&mut response);
            return Ok(response);
        }
        // A local macro to simplify error handling. If a Result is an Err,
        // it immediately formats it into a JSON-RPC error response and returns.
        macro_rules! unwrap {
            ($result:expr, $id: expr) => {
                match $result {
                    Ok(r) => r,
                    Err(error) => {
                        let mut resp = ResponseErrorPayload::encode($id, error);
                        Self::set_access_control_headers(&mut resp);
                        return Ok(resp);
                    }
                }
            };
            (@noret, $result:expr, $id: expr) => {
                match $result {
                    Ok(r) => r,
                    Err(error) => {
                        let mut resp = ResponseErrorPayload::encode($id, error);
                        Self::set_access_control_headers(&mut resp);
                        resp
                    }
                }
            };
        }

        // Extract and parse the request body.
        let body = unwrap!(extract_bytes(request).await, None);
        let request = unwrap!(parse_body(body), None);

        // Resolve the handler for request and process it
        let (response, id) = match request {
            RpcRequest::Single(mut r) => {
                let response = self.process(&mut r).await;
                (response, Some(r.id))
            }
            RpcRequest::Multi(requests) => {
                const COMA: u8 = b',';
                const OPEN_BR: u8 = b'[';
                const CLOSE_BR: u8 = b']';
                let mut jobs = FuturesOrdered::new();
                for mut r in requests {
                    let j = async {
                        let response = self.process(&mut r).await;
                        (response, r)
                    };
                    jobs.push_back(j);
                }
                let mut body = vec![OPEN_BR];
                while let Some((response, request)) = jobs.next().await {
                    if body.len() != 1 {
                        body.push(COMA);
                    }
                    let response = unwrap!(@noret, response, Some(&request.id));
                    body.extend_from_slice(&response.into_body().0);
                }
                body.push(CLOSE_BR);
                (Ok(Response::new(JsonBody(body))), None)
            }
        };

        // Handle any errors from the handling stage
        let mut response = unwrap!(response, id.as_ref());
        Self::set_access_control_headers(&mut response);
        Ok(response)
    }

    async fn process(&self, request: &mut JsonHttpRequest) -> HandlerResult {
        // Route the request to the correct handler based on the method name.
        use crate::requests::JsonRpcHttpMethod::*;
        let method = request.method.as_str();
        RPC_REQUESTS_COUNT.with_label_values(&[method]).inc();
        let _timer = RPC_REQUEST_HANDLING_TIME
            .with_label_values(&[method])
            .start_timer();

        match request.method {
            GetAccountInfo => self.get_account_info(request).await,
            GetBalance => self.get_balance(request).await,
            GetBlock => self.get_block(request),
            GetBlockCommitment => self.get_block_commitment(request),
            GetBlockHeight => self.get_block_height(request),
            GetBlockTime => self.get_block_time(request),
            GetBlocks => self.get_blocks(request),
            GetBlocksWithLimit => self.get_blocks_with_limit(request),
            GetClusterNodes => self.get_cluster_nodes(request),
            GetEpochInfo => self.get_epoch_info(request),
            GetEpochSchedule => self.get_epoch_schedule(request),
            GetFeeForMessage => self.get_fee_for_message(request),
            GetFirstAvailableBlock => self.get_first_available_block(request),
            GetGenesisHash => self.get_genesis_hash(request),
            GetHealth => self.get_health(request),
            GetHighestSnapshotSlot => self.get_highest_snapshot_slot(request),
            GetIdentity => self.get_identity(request),
            GetLargestAccounts => self.get_largest_accounts(request),
            GetLatestBlockhash => self.get_latest_blockhash(request),
            GetMultipleAccounts => self.get_multiple_accounts(request).await,
            GetProgramAccounts => self.get_program_accounts(request),
            GetRecentPerformanceSamples => {
                self.get_recent_performance_samples(request)
            }
            GetSignatureStatuses => self.get_signature_statuses(request),
            GetSignaturesForAddress => self.get_signatures_for_address(request),
            GetSlot => self.get_slot(request),
            GetSlotLeader => self.get_slot_leader(request),
            GetSlotLeaders => self.get_slot_leaders(request),
            GetSupply => self.get_supply(request),
            GetTokenAccountBalance => {
                self.get_token_account_balance(request).await
            }
            GetTokenAccountsByDelegate => {
                self.get_token_accounts_by_delegate(request)
            }
            GetTokenAccountsByOwner => {
                self.get_token_accounts_by_owner(request)
            }
            GetTokenLargestAccounts => self.get_token_largest_accounts(request),
            GetTokenSupply => self.get_token_supply(request),
            GetTransaction => self.get_transaction(request),
            GetTransactionCount => self.get_transaction_count(request),
            GetVersion => self.get_version(request),
            GetVoteAccounts => self.get_vote_accounts(request),
            IsBlockhashValid => self.is_blockhash_valid(request),
            MinimumLedgerSlot => self.get_first_available_block(request),
            RequestAirdrop => self.request_airdrop(request).await,
            SendTransaction => self.send_transaction(request).await,
            SimulateTransaction => self.simulate_transaction(request).await,
            GetRoutes => self.get_routes(request),
            GetBlockhashForAccounts => self.get_blockhash_for_accounts(request),
            GetDelegationStatus => self.get_delegation_status(request).await,
        }
    }

    /// Set CORS/Access control related headers (required by explorers/web apps)
    fn set_access_control_headers(response: &mut Response<JsonBody>) {
        static HV: fn(&'static str) -> HeaderValue =
            |v| HeaderValue::from_static(v);

        let headers = response.headers_mut();

        headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, HV("*"));
        headers.insert(ACCESS_CONTROL_ALLOW_METHODS, HV("POST, OPTIONS, GET"));
        headers.insert(ACCESS_CONTROL_ALLOW_HEADERS, HV("*"));
        headers.insert(ACCESS_CONTROL_MAX_AGE, HV("86400"));
    }
}
