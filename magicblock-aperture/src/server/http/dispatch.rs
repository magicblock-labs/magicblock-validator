use core::str;
use std::{collections::HashMap, convert::Infallible, str::FromStr, sync::Arc};

use futures::{stream::FuturesOrdered, StreamExt};
use hyper::{
    body::Incoming,
    header::{
        HeaderValue, ACCESS_CONTROL_ALLOW_HEADERS,
        ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
        ACCESS_CONTROL_MAX_AGE,
    },
    Method, Request, Response, StatusCode,
};
use magicblock_accounts_db::AccountsDb;
use magicblock_core::{
    coordination_mode::CoordinationMode,
    link::{transactions::TransactionSchedulerHandle, DispatchEndpoints},
};
use magicblock_ledger::Ledger;
use magicblock_metrics::metrics::{
    RPC_REQUESTS_COUNT, RPC_REQUEST_HANDLING_TIME,
};
use magicblock_query_filtering::{
    auth::AuthError, quote, service::QueryFilteringError, types::LoginRequest,
    QueryFilteringService,
};
use solana_pubkey::Pubkey;

use crate::{
    requests::{
        http::{extract_bytes, parse_body, Data, HandlerResult},
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
    /// Query filtering service and local authentication state.
    pub(crate) query_filtering: Option<Arc<QueryFilteringService>>,
}

impl HttpDispatcher {
    /// Creates a new, thread-safe `HttpDispatcher` instance.
    ///
    /// This constructor clones the necessary handles from the global `SharedState` and
    /// `DispatchEndpoints`, making it cheap to create multiple `Arc<Self>` pointers.
    pub(super) fn new(
        query_filtering: Option<Arc<QueryFilteringService>>,
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
            query_filtering,
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
        let request = match self.handle_special_request(request).await {
            Ok(response) => return Ok(response),
            Err(request) => request,
        };

        // Extract the authenticated user from the request.
        let authenticated_user =
            if let Some(query_filtering) = &self.query_filtering {
                let Some(token) = extract_token(&request) else {
                    let response = cors_json_response(
                        StatusCode::UNAUTHORIZED,
                        "missing auth token",
                    );
                    return Ok(response);
                };
                let pubkey = match query_filtering.verify_token(&token) {
                    Ok(pubkey) => pubkey,
                    Err(err) => {
                        let response = cors_json_response(
                            StatusCode::UNAUTHORIZED,
                            &err.to_string(),
                        );
                        return Ok(response);
                    }
                };
                match pubkey.parse::<Pubkey>() {
                    Ok(pubkey) => Some(pubkey),
                    Err(err) => {
                        let response = cors_json_response(
                            StatusCode::UNAUTHORIZED,
                            &err.to_string(),
                        );
                        return Ok(response);
                    }
                }
            } else {
                None
            };

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
                r.authenticated_user = authenticated_user;
                let response = self.process(&mut r).await;
                (response, Some(r.id))
            }
            RpcRequest::Multi(requests) => {
                const COMA: u8 = b',';
                const OPEN_BR: u8 = b'[';
                const CLOSE_BR: u8 = b']';
                let mut jobs = FuturesOrdered::new();
                for mut r in requests {
                    r.authenticated_user = authenticated_user;
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
            GetProgramAccounts => self.get_program_accounts(request).await,
            GetRecentPerformanceSamples => {
                self.get_recent_performance_samples(request)
            }
            GetSignatureStatuses => self.get_signature_statuses(request),
            GetSignaturesForAddress => {
                self.get_signatures_for_address(request).await
            }
            GetSlot => self.get_slot(request),
            GetSlotLeader => self.get_slot_leader(request),
            GetSlotLeaders => self.get_slot_leaders(request),
            GetSupply => self.get_supply(request),
            GetTokenAccountBalance => {
                self.get_token_account_balance(request).await
            }
            GetTokenAccountsByDelegate => {
                self.get_token_accounts_by_delegate(request).await
            }
            GetTokenAccountsByOwner => {
                self.get_token_accounts_by_owner(request).await
            }
            GetTokenLargestAccounts => self.get_token_largest_accounts(request),
            GetTokenSupply => self.get_token_supply(request),
            GetTransaction => self.get_transaction(request).await,
            GetTransactionCount => self.get_transaction_count(request),
            GetVersion => self.get_version(request),
            GetVoteAccounts => self.get_vote_accounts(request),
            IsBlockhashValid => self.is_blockhash_valid(request),
            MinimumLedgerSlot => self.get_first_available_block(request),
            RequestAirdrop => self.request_airdrop(request).await,
            SendTransaction => self.send_transaction(request).await,
            SimulateTransaction => self.simulate_transaction(request).await,
            GetRoutes => self.get_routes(request),
            // Alias for getLatestBlockhash; exists for Magic Router SDK compatibility.
            GetBlockhashForAccounts => self.get_latest_blockhash(request),
            GetDelegationStatus => self.get_delegation_status(request).await,
        }
    }

    async fn handle_special_request(
        &self,
        request: Request<Incoming>,
    ) -> Result<Response<JsonBody>, Request<Incoming>> {
        if request.method() == Method::OPTIONS {
            let mut response = Response::new(JsonBody::from(""));
            Self::set_access_control_headers(&mut response);
            return Ok(response);
        } else if request.uri() == "/health/primary" {
            let mut response = Response::new(JsonBody::from(""));
            Self::set_access_control_headers(&mut response);
            if CoordinationMode::current() != CoordinationMode::Primary {
                *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE
            }
            return Ok(response);
        }

        // Functions below are only available if query filtering is enabled.
        let Some(query_filtering) = &self.query_filtering else {
            return Err(request);
        };

        if request.method() == Method::GET && request.uri().path() == "/quote" {
            return Ok(Self::quote_response(&request, false).await);
        }
        if request.method() == Method::GET
            && request.uri().path() == "/fast-quote"
        {
            return Ok(Self::quote_response(&request, true).await);
        }
        if request.method() == Method::GET
            && request.uri().path() == "/auth/challenge"
        {
            return Ok(self.challenge_response(query_filtering, &request));
        }
        if request.method() == Method::POST
            && request.uri().path() == "/auth/login"
        {
            return Ok(self.login_response(query_filtering, request).await);
        }

        Err(request)
    }

    async fn quote_response(
        request: &Request<Incoming>,
        fast: bool,
    ) -> Response<JsonBody> {
        let Some(challenge) = query_params(request).remove("challenge") else {
            let mut response =
                json_response(StatusCode::BAD_REQUEST, "missing challenge");
            Self::set_access_control_headers(&mut response);
            return response;
        };

        let mut response = if fast {
            match quote::fast_quote(&challenge).await {
                Ok(body) => quote_json_response(&body),
                Err(err) => {
                    json_response(quote_error_status(&err), &err.to_string())
                }
            }
        } else {
            match quote::quote(&challenge).await {
                Ok(body) => quote_json_response(&body),
                Err(err) => {
                    json_response(quote_error_status(&err), &err.to_string())
                }
            }
        };
        Self::set_access_control_headers(&mut response);
        response
    }

    fn challenge_response(
        &self,
        query_filtering: &QueryFilteringService,
        request: &Request<Incoming>,
    ) -> Response<JsonBody> {
        let Some(pubkey) = query_params(request).remove("pubkey") else {
            return cors_json_response(
                StatusCode::BAD_REQUEST,
                "missing pubkey",
            );
        };

        // Check if the pubkey is valid
        if let Err(err) = Pubkey::from_str(&pubkey) {
            return cors_json_response(
                auth_error_status(&err.clone().into()),
                &err.to_string(),
            );
        };

        cors_quote_json_response(&query_filtering.get_challenge(&pubkey))
    }

    async fn login_response(
        &self,
        query_filtering: &QueryFilteringService,
        request: Request<Incoming>,
    ) -> Response<JsonBody> {
        let body = match extract_bytes(request).await {
            Ok(body) => body,
            Err(err) => {
                return cors_json_response(
                    StatusCode::BAD_REQUEST,
                    &err.to_string(),
                )
            }
        };
        let body = body_data_to_vec(body);
        let request = match serde_json::from_slice::<LoginRequest>(&body) {
            Ok(request) => request,
            Err(err) => {
                return cors_json_response(
                    StatusCode::BAD_REQUEST,
                    &err.to_string(),
                )
            }
        };

        match query_filtering.login(request).await {
            Ok(body) => cors_quote_json_response(&body),
            Err(err) => cors_json_response(
                query_filtering_auth_error_status(&err),
                &err.to_string(),
            ),
        }
    }

    /// Set CORS/Access control related headers (required by explorers/web apps)
    fn set_access_control_headers(response: &mut Response<JsonBody>) {
        const fn hv(v: &'static str) -> HeaderValue {
            HeaderValue::from_static(v)
        }

        let headers = response.headers_mut();

        headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, hv("*"));
        headers.insert(ACCESS_CONTROL_ALLOW_METHODS, hv("POST, OPTIONS, GET"));
        headers.insert(ACCESS_CONTROL_ALLOW_HEADERS, hv("*"));
        headers.insert(ACCESS_CONTROL_MAX_AGE, hv("86400"));
    }
}

fn query_params<B>(req: &Request<B>) -> HashMap<String, String> {
    req.uri()
        .query()
        .map(|q| {
            url::form_urlencoded::parse(q.as_bytes())
                .into_owned()
                .collect()
        })
        .unwrap_or_default()
}

fn extract_token<B>(req: &Request<B>) -> Option<String> {
    query_params(req).get("token").cloned().or_else(|| {
        req.headers()
            .get("Authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| {
                let mut parts = value.split_whitespace();
                match (parts.next(), parts.next(), parts.next()) {
                    (Some(scheme), Some(token), None)
                        if scheme.eq_ignore_ascii_case("Bearer") =>
                    {
                        Some(token.to_owned())
                    }
                    _ => None,
                }
            })
    })
}

fn json_response(status: StatusCode, error: &str) -> Response<JsonBody> {
    let body = serde_json::json!({ "error": error });
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(JsonBody(serde_json::to_vec(&body).unwrap_or_default()))
        .unwrap_or_else(|_| Response::new(JsonBody(Vec::new())))
}

fn quote_json_response<T: serde::Serialize>(body: &T) -> Response<JsonBody> {
    match serde_json::to_vec(body) {
        Ok(body) => Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(JsonBody(body))
            .unwrap_or_else(|_| Response::new(JsonBody(Vec::new()))),
        Err(err) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("failed to serialize quote response: {err}"),
        ),
    }
}

fn cors_json_response(status: StatusCode, error: &str) -> Response<JsonBody> {
    let mut response = json_response(status, error);
    HttpDispatcher::set_access_control_headers(&mut response);
    response
}

fn cors_quote_json_response<T: serde::Serialize>(
    body: &T,
) -> Response<JsonBody> {
    let mut response = quote_json_response(body);
    HttpDispatcher::set_access_control_headers(&mut response);
    response
}

fn body_data_to_vec(body: Data) -> Vec<u8> {
    match body {
        Data::Empty => Vec::new(),
        Data::SingleChunk(bytes) => bytes.to_vec(),
        Data::MultiChunk(bytes) => bytes,
    }
}

fn quote_error_status(error: &quote::QuoteError) -> StatusCode {
    match error {
        quote::QuoteError::InvalidChallenge | quote::QuoteError::Base64(_) => {
            StatusCode::BAD_REQUEST
        }
        quote::QuoteError::TdxGuest(_) => StatusCode::NOT_IMPLEMENTED,
        quote::QuoteError::Join(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn auth_error_status(error: &AuthError) -> StatusCode {
    match error {
        AuthError::Json(_)
        | AuthError::ParsePubkey(_)
        | AuthError::ParseSignature(_)
        | AuthError::InvalidChallengeDate
        | AuthError::InvalidChallengeFormat
        | AuthError::InvalidChallengeUserPubkey
        | AuthError::InvalidChallengeTtlSeconds => StatusCode::BAD_REQUEST,
        AuthError::ChallengeExpired
        | AuthError::SignatureVerification
        | AuthError::TokenExpired => StatusCode::UNAUTHORIZED,
        AuthError::Jwt(_) | AuthError::Risk(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

fn query_filtering_auth_error_status(
    error: &QueryFilteringError,
) -> StatusCode {
    match error {
        QueryFilteringError::Auth(error) => auth_error_status(error),
        QueryFilteringError::Permission { .. } => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
        QueryFilteringError::AccessDenied => StatusCode::UNAUTHORIZED,
    }
}

#[cfg(test)]
mod tests {
    use hyper::header::AUTHORIZATION;

    use super::*;

    #[test]
    fn extract_token_uses_query_param() {
        let request = Request::builder()
            .uri("/?token=query-token")
            .body(())
            .unwrap();

        assert_eq!(extract_token(&request).as_deref(), Some("query-token"));
    }

    #[test]
    fn extract_token_uses_bearer_header() {
        let request = Request::builder()
            .uri("/")
            .header(AUTHORIZATION, "Bearer header-token")
            .body(())
            .unwrap();

        assert_eq!(extract_token(&request).as_deref(), Some("header-token"));
    }
}
