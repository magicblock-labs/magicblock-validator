use core::str;
use std::{convert::Infallible, sync::Arc};

use engine::Engine;
use futures::{StreamExt, stream::FuturesOrdered};
use http_body_util::BodyExt;
use hyper::{
    Method, Request, Response,
    body::{Bytes, Incoming},
    header::{
        ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
        ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_MAX_AGE, HeaderName,
        HeaderValue,
    },
};
use magicblock_chainlink::ProdChainlink;
use magicblock_ledger_deprecated::Ledger;
use magicblock_metrics::metrics::{
    RPC_REQUEST_HANDLING_TIME, RPC_REQUESTS_COUNT,
};

use crate::{
    error::RpcError,
    requests::{
        JsonHttpRequest, RpcRequest,
        http::{
            ClaimedHandlerResult, HandlerResult, get_blocks::BlockRange,
            get_token_accounts::TokenAccountAuthority,
        },
        payload::{JsonBody, ResponseErrorPayload},
    },
    state::SharedState,
};

const REMOTE_ACCOUNT_CLAIMS_HEADER: HeaderName =
    HeaderName::from_static("x-mb-remote-account-claims");

enum Data {
    Empty,
    Single(Bytes),
    Multi(Vec<u8>),
}

impl Data {
    fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Single(data) => data.len(),
            Self::Multi(data) => data.len(),
        }
    }
}

fn parse_body(body: Data) -> crate::RpcResult<RpcRequest> {
    let bytes = match &body {
        Data::Empty => {
            return Err(RpcError::invalid_request("missing request body"));
        }
        Data::Single(data) => data.as_ref(),
        Data::Multi(data) => data.as_ref(),
    }
    .trim_ascii_start();
    if bytes.first() == Some(&b'{') {
        json::from_slice(bytes).map(RpcRequest::Single)
    } else {
        json::from_slice(bytes).map(RpcRequest::Multi)
    }
    .map_err(Into::into)
}

async fn extract_bytes(request: Request<Incoming>) -> crate::RpcResult<Data> {
    const MAX_BODY_SIZE: usize = 1024 * 1024;
    let mut body = request.into_body();
    let mut data = Data::Empty;
    while let Some(frame) = body.frame().await {
        let Ok(chunk) = frame?.into_data() else {
            continue;
        };
        match &mut data {
            Data::Empty => data = Data::Single(chunk),
            Data::Single(first) => {
                let mut buffer = Vec::with_capacity(first.len() + chunk.len());
                buffer.extend_from_slice(first);
                buffer.extend_from_slice(&chunk);
                data = Data::Multi(buffer);
            }
            Data::Multi(buffer) => buffer.extend_from_slice(&chunk),
        }
        if data.len() > MAX_BODY_SIZE {
            return Err(RpcError::invalid_request(
                "request body exceed 1MiB limit",
            ));
        }
    }
    Ok(data)
}

pub(crate) struct HttpDispatcher {
    pub(crate) blocktime_ms: u64,
    /// The engine: account/block/transaction reads and transaction submission.
    pub(crate) engine: Engine,
    /// Read-only deprecated ledger, used as a fallback for historical reads.
    pub(crate) ledger: Arc<Ledger>,
    /// Chainlink provides synchronization of on-chain accounts and
    /// fetches accounts used in a specific transaction as well as those
    /// required when getting account info, etc.
    pub(crate) chainlink: Arc<ProdChainlink>,
}

impl HttpDispatcher {
    pub(super) fn new(state: SharedState) -> Arc<Self> {
        Arc::new(Self {
            blocktime_ms: state.blocktime_ms,
            engine: state.engine,
            ledger: state.ledger,
            chainlink: state.chainlink,
        })
    }

    pub(super) async fn dispatch(
        self: Arc<Self>,
        request: Request<Incoming>,
    ) -> Result<Response<JsonBody>, Infallible> {
        // bounce back control requests
        let response = (request.method() == Method::OPTIONS).then(|| {
            let mut response = Response::new(JsonBody::from(""));
            Self::set_headers(&mut response, 0);
            response
        });
        if let Some(response) = response {
            return Ok(response);
        }

        let request = match extract_bytes(request).await.and_then(parse_body) {
            Ok(request) => request,
            Err(error) => {
                let mut response = ResponseErrorPayload::encode(None, error);
                Self::set_headers(&mut response, 0);
                return Ok(response);
            }
        };

        // Resolve the handler for request and process it
        let (mut response, claims) = match request {
            RpcRequest::Single(r) => {
                let (result, claims) = self.process(&r).await;
                let response = result.unwrap_or_else(|error| {
                    ResponseErrorPayload::encode(Some(&r.id), error)
                });
                (response, claims)
            }
            RpcRequest::Multi(requests) => {
                const COMA: u8 = b',';
                const OPEN_BR: u8 = b'[';
                const CLOSE_BR: u8 = b']';
                let mut jobs = FuturesOrdered::new();
                for r in requests {
                    let dispatcher = self.clone();
                    let j = async move {
                        let response = dispatcher.process(&r).await;
                        (response, r)
                    };
                    jobs.push_back(j);
                }
                let mut body = vec![OPEN_BR];
                let mut claims = 0;
                while let Some(((response, request_claims), request)) =
                    jobs.next().await
                {
                    claims += request_claims;
                    if body.len() != 1 {
                        body.push(COMA);
                    }
                    let response = response.unwrap_or_else(|error| {
                        ResponseErrorPayload::encode(Some(&request.id), error)
                    });
                    body.extend_from_slice(&response.into_body().0);
                }
                body.push(CLOSE_BR);
                (Response::new(JsonBody(body)), claims)
            }
        };
        Self::set_headers(&mut response, claims);
        Ok(response)
    }

    async fn process(&self, request: &JsonHttpRequest) -> ClaimedHandlerResult {
        // Route the request to the correct handler based on the method name.
        use crate::requests::JsonRpcHttpMethod::*;
        let method = request.method.as_str();
        RPC_REQUESTS_COUNT.with_label_values(&[method]).inc();
        let _timer = RPC_REQUEST_HANDLING_TIME
            .with_label_values(&[method])
            .start_timer();

        let result: HandlerResult = match request.method {
            GetAccountInfo => return self.get_account_info(request).await,
            GetBalance => return self.get_balance(request).await,
            GetBlock => self.get_block(request).await,
            GetBlockCommitment => self.get_block_commitment(request),
            GetBlockHeight => self.get_slot(request),
            GetBlockTime => self.get_block_time(request).await,
            GetBlocks => self.get_blocks(request, BlockRange::EndSlot),
            GetBlocksWithLimit => self.get_blocks(request, BlockRange::Limit),
            GetClusterNodes => self.get_cluster_nodes(request),
            GetEpochInfo => self.get_epoch_info(request),
            GetEpochSchedule => self.get_epoch_schedule(request),
            GetFeeForMessage => self.get_fee_for_message(request),
            GetFirstAvailableBlock => self.mock_zero(request),
            GetGenesisHash => self.get_genesis_hash(request),
            GetHealth => self.get_health(request),
            GetHighestSnapshotSlot => self.get_highest_snapshot_slot(request),
            GetIdentity => self.get_identity(request),
            GetLargestAccounts => self.mock_empty_context(request),
            GetLatestBlockhash => self.get_latest_blockhash(request),
            GetMultipleAccounts => {
                return self.get_multiple_accounts(request).await;
            }
            GetProgramAccounts => self.get_program_accounts(request),
            GetRecentPerformanceSamples => {
                self.get_recent_performance_samples(request)
            }
            GetSignatureStatuses => self.get_signature_statuses(request).await,
            GetSignaturesForAddress => {
                self.get_signatures_for_address(request).await
            }
            GetSlot => self.get_slot(request),
            GetSlotLeader => self.get_slot_leader(request),
            GetSlotLeaders => self.get_slot_leaders(request),
            GetSupply => self.get_supply(request),
            GetTokenAccountBalance => {
                return self.get_token_account_balance(request).await;
            }
            GetTokenAccountsByDelegate => self
                .get_token_accounts(request, TokenAccountAuthority::Delegate),
            GetTokenAccountsByOwner => {
                self.get_token_accounts(request, TokenAccountAuthority::Owner)
            }
            GetTokenLargestAccounts => self.mock_empty_context(request),
            GetTokenSupply => self.get_token_supply(request),
            GetTransaction => self.get_transaction(request).await,
            GetTransactionCount => self.mock_zero(request),
            GetVersion => self.get_version(request),
            GetVoteAccounts => self.get_vote_accounts(request),
            IsBlockhashValid => self.is_blockhash_valid(request),
            MinimumLedgerSlot => self.mock_zero(request),
            RequestAirdrop => self.request_airdrop(request).await,
            SendTransaction => return self.send_transaction(request).await,
            SimulateTransaction => {
                return self.simulate_transaction(request).await;
            }
            GetRoutes => self.mock_empty(request),
            // Alias for getLatestBlockhash; exists for Magic Router SDK compatibility.
            GetBlockhashForAccounts => self.get_latest_blockhash(request),
            GetDelegationStatus => {
                return self.get_delegation_status(request).await;
            }
            MethodNotFound => Err(RpcError::method_not_found()),
        };
        (result, 0)
    }

    /// Set CORS/Access control related headers (required by explorers/web apps)
    /// and the custom header to count the number of remote account requests
    fn set_headers(response: &mut Response<JsonBody>, claims: u64) {
        const fn hv(v: &'static str) -> HeaderValue {
            HeaderValue::from_static(v)
        }

        let headers = response.headers_mut();
        if let Ok(val) = HeaderValue::from_str(&claims.to_string()) {
            headers.insert(REMOTE_ACCOUNT_CLAIMS_HEADER, val);
        }

        headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, hv("*"));
        headers.insert(ACCESS_CONTROL_ALLOW_METHODS, hv("POST, OPTIONS, GET"));
        headers.insert(ACCESS_CONTROL_ALLOW_HEADERS, hv("*"));
        headers.insert(ACCESS_CONTROL_MAX_AGE, hv("86400"));
    }
}
