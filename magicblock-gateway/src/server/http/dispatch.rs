use std::{convert::Infallible, sync::Arc};

use hyper::{body::Incoming, Request, Response};
use magicblock_accounts_db::AccountsDb;
use magicblock_core::link::{
    transactions::TransactionSchedulerHandle, DispatchEndpoints,
};
use magicblock_ledger::Ledger;
use solana_pubkey::Pubkey;

use crate::{
    requests::{
        http::{extract_bytes, parse_body, HandlerResult},
        payload::ResponseErrorPayload,
        JsonHttpRequest,
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

        // Extract and parse the request body.
        let body = unwrap!(extract_bytes(request).await, None);
        let mut request = unwrap!(parse_body(body), None);
        // Resolve the handler for request and process it
        let response = self.process(&mut request).await;

        // Format the final response, handling any errors from the execution stage.
        Ok(unwrap!(response, Some(&request.id)))
    }

    async fn process(&self, request: &mut JsonHttpRequest) -> HandlerResult {
        // Route the request to the correct handler based on the method name.
        use crate::requests::JsonRpcHttpMethod::*;
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
            GetFirstAvailableBlock => self.get_first_available_block(request),
            GetGenesisHash => self.get_genesis_hash(request),
            GetHealth => self.get_health(request),
            GetHighestSnapshotSlot => self.get_highest_snapshot_slot(request),
            GetIdentity => self.get_identity(request),
            GetLargestAccounts => self.get_largest_accounts(request),
            GetLatestBlockhash => self.get_latest_blockhash(request),
            GetMultipleAccounts => self.get_multiple_accounts(request).await,
            GetProgramAccounts => self.get_program_accounts(request),
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
            GetVersion => self.get_version(request),
            IsBlockhashValid => self.is_blockhash_valid(request),
            MinimumLedgerSlot => self.get_first_available_block(request),
            SendTransaction => self.send_transaction(request).await,
            SimulateTransaction => self.simulate_transaction(request).await,
        }
    }
}
