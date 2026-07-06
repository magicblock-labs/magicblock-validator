use core::str;
use std::{mem::size_of, ops::Range, sync::Arc, time::Duration};

use base64::{Engine, prelude::BASE64_STANDARD};
use http_body_util::BodyExt;
use hyper::{
    Request, Response,
    body::{Bytes, Incoming},
};
use magicblock_chainlink::errors::ChainlinkError;
use magicblock_core::Slot;
use magicblock_metrics::metrics::{AccountFetchContext, ENSURE_ACCOUNTS_TIME};
use nucleus::runtime::TransactionView;
use prelude::JsonBody;
use solana_account::{AccountMode, AccountSharedData, ReadableAccount};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::response::RpcBlockhash;
use solana_transaction_status::UiTransactionEncoding;
use tracing::*;

use super::RpcRequest;
use crate::{
    RpcResult, error::RpcError, server::http::dispatch::HttpDispatcher,
};

pub(crate) type HandlerResult = RpcResult<Response<JsonBody>>;

const SYSTEM_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("11111111111111111111111111111111");

/// Standard Solana slot time in milliseconds, used to scale blockhash validity
/// to the ephemeral validator's (typically faster) block production rate.
const SOLANA_BLOCK_TIME_MS: f64 = 400.0;
/// Number of slots a blockhash is considered valid on Solana mainnet.
const MAX_VALID_BLOCKHASH_SLOTS: f64 = 150.0;

/// An enum to efficiently represent a request body, avoiding allocation
/// for single-chunk bodies (which are almost always the case)
pub(crate) enum Data {
    Empty,
    SingleChunk(Bytes),
    MultiChunk(Vec<u8>),
}

impl Data {
    fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::SingleChunk(b) => b.len(),
            Self::MultiChunk(b) => b.len(),
        }
    }
}

/// Deserializes the raw request body bytes into a structured `JsonHttpRequest`.
pub(crate) fn parse_body(body: Data) -> RpcResult<RpcRequest> {
    let body_bytes = match &body {
        Data::Empty => {
            return Err(RpcError::invalid_request("missing request body"));
        }
        Data::SingleChunk(slice) => slice.as_ref(),
        Data::MultiChunk(vec) => vec.as_ref(),
    }
    .trim_ascii_start();
    // Hacky/cheap way to detect single request vs an array of requests
    if body_bytes.first().map(|&b| b == b'{').unwrap_or_default() {
        json::from_slice(body_bytes).map(RpcRequest::Single)
    } else {
        json::from_slice(body_bytes).map(RpcRequest::Multi)
    }
    .map_err(Into::into)
}

/// Asynchronously reads all data from an HTTP request body, correctly handling chunked transfers.
pub(crate) async fn extract_bytes(
    request: Request<Incoming>,
) -> RpcResult<Data> {
    const MAX_BODY_SIZE: usize = 1024 * 1024; // 1MiB
    let mut body = request.into_body();
    let mut data = Data::Empty;

    // This loop efficiently accumulates body chunks. It starts with a zero-copy
    // `SingleChunk` and only allocates and copies to a `MultiChunk` `Vec` if a
    // second chunk arrives.
    while let Some(next) = body.frame().await {
        let Ok(chunk) = next?.into_data() else {
            continue;
        };
        match &mut data {
            Data::Empty => data = Data::SingleChunk(chunk),
            Data::SingleChunk(first) => {
                let mut buffer = Vec::with_capacity(first.len() + chunk.len());
                buffer.extend_from_slice(first);
                buffer.extend_from_slice(&chunk);
                data = Data::MultiChunk(buffer);
            }
            Data::MultiChunk(buffer) => {
                buffer.extend_from_slice(&chunk);
            }
        }
        if data.len() > MAX_BODY_SIZE {
            return Err(RpcError::invalid_request(
                "request body exceed 1MiB limit",
            ));
        }
    }
    Ok(data)
}

/// # HTTP Dispatcher Helpers
///
/// This block contains common helper methods used by various RPC request handlers.
impl HttpDispatcher {
    /// Builds an [`RpcBlockhash`] from the engine's latest block, returning it
    /// alongside that block's slot. The validity window is scaled from the
    /// mainnet 150-slot bound by the ratio of standard to actual block time.
    fn latest_blockhash(&self) -> (RpcBlockhash, Slot) {
        let block = self.engine.blocks().latest();
        let ratio = SOLANA_BLOCK_TIME_MS / self.context.blocktime.max(1) as f64;
        let validity = (ratio * MAX_VALID_BLOCKHASH_SLOTS) as u64;
        let response = RpcBlockhash {
            blockhash: block.hash.to_string(),
            last_valid_block_height: block.slot + validity,
        };
        (response, block.slot)
    }

    // Heuristic to render synthetic empty placeholder accounts as JSON-RPC null.
    fn account_should_render_as_null(account: &AccountSharedData) -> bool {
        account.lamports() == 0
            && account.data().is_empty()
            && !account.is(AccountMode::Delegated)
            && !account.is(AccountMode::Transient)
            && !account.is(AccountMode::Ephemeral)
            && account.owner() == &SYSTEM_PROGRAM_ID
    }

    fn needs_onchain_interactions(&self) -> bool {
        self.context.is_primary
    }

    fn require_primary_rpc_method(
        &self,
        method: &'static str,
    ) -> RpcResult<()> {
        if self.needs_onchain_interactions() {
            Ok(())
        } else {
            Err(RpcError::transaction_verification(format!(
                "{method} is only available while validator is primary"
            )))
        }
    }

    /// Reads an account through the engine, first ensuring it is cloned in
    /// from chain as needed.
    #[instrument(skip_all)]
    async fn read_account_with_ensure(
        &self,
        pubkey: &Pubkey,
    ) -> Option<AccountSharedData> {
        let mark_empty_if_not_found = [*pubkey];
        let _timer = ENSURE_ACCOUNTS_TIME
            .with_label_values(&["account"])
            .start_timer();
        let _ = self
            .chainlink
            .ensure_accounts(
                &[*pubkey],
                Some(&mark_empty_if_not_found),
                AccountFetchContext::rpc_get_account(),
            )
            .await
            .inspect_err(|e| {
                // There is nothing we can do if fetching the account fails
                // Log the error and return whatever the engine holds
                debug!(error = ?e, "Failed to ensure account");
            });
        self.engine.accounts().get(pubkey).ok().flatten()
    }

    /// Reads multiple accounts through the engine, first ensuring they are
    /// cloned in from chain as needed.
    #[instrument(skip(self, pubkeys), fields(pubkey_count = pubkeys.len()))]
    async fn read_accounts_with_ensure(
        &self,
        pubkeys: &[Pubkey],
    ) -> Vec<Option<AccountSharedData>> {
        trace!("Ensuring accounts");
        let _timer = ENSURE_ACCOUNTS_TIME
            .with_label_values(&["multi-account"])
            .start_timer();
        let _ = self
            .chainlink
            .ensure_accounts(
                pubkeys,
                Some(pubkeys),
                AccountFetchContext::rpc_get_multiple_accounts(),
            )
            .await
            .inspect_err(|e| {
                // There is nothing we can do if fetching the accounts fails
                // Log the error and return whatever the engine holds
                warn!(error = ?e, "Failed to ensure accounts");
            });
        pubkeys
            .iter()
            .map(|pubkey| self.engine.accounts().get(pubkey).ok().flatten())
            .collect()
    }

    /// Decodes a wire-format transaction string into a sanitized
    /// [`TransactionView`] over the raw signed bytes.
    ///
    /// The engine owns verification, replay protection and blockhash
    /// validation, so this performs no signature or blockhash checks — it only
    /// decodes and sanitizes the structure so the accounts required by the
    /// transaction can be resolved and the payload handed to the engine.
    fn decode_transaction(
        &self,
        txn: &str,
        encoding: UiTransactionEncoding,
    ) -> RpcResult<TransactionView> {
        let bytes = match encoding {
            UiTransactionEncoding::Base58 => bs58::decode(txn)
                .into_vec()
                .map_err(RpcError::parse_error)?,
            UiTransactionEncoding::Base64 => {
                BASE64_STANDARD.decode(txn).map_err(RpcError::parse_error)?
            }
            _ => {
                return Err(RpcError::invalid_params(
                    "unsupported transaction encoding",
                ));
            }
        };
        TransactionView::try_new_sanitized(Arc::new(bytes), true)
            .map_err(|e| RpcError::invalid_params(format!("{e:?}")))
    }

    /// Ensures all accounts required for a transaction are cloned in via the
    /// chainlink before the transaction is handed to the engine.
    #[instrument(skip_all)]
    async fn ensure_transaction_accounts(
        &self,
        transaction: &TransactionView,
    ) -> RpcResult<()> {
        // Hard bound on account preparation: if the cloning pipeline is
        // degraded the transaction must fail with an error instead of
        // holding the request (and the client) hostage indefinitely.
        const ENSURE_ACCOUNTS_TIMEOUT: Duration = Duration::from_secs(30);

        self.require_primary_rpc_method("transaction")?;

        let _timer = ENSURE_ACCOUNTS_TIME
            .with_label_values(&["transaction"])
            .start_timer();
        match tokio::time::timeout(
            ENSURE_ACCOUNTS_TIMEOUT,
            self.chainlink.ensure_transaction_accounts(transaction),
        )
        .await
        .unwrap_or_else(|_elapsed| {
            Err(ChainlinkError::EnsureAccountsTimeout(
                ENSURE_ACCOUNTS_TIMEOUT.as_secs(),
            ))
        }) {
            Ok(res) if res.is_ok() => Ok(()),
            Ok(res) => {
                debug!(%res, "Transaction account resolution encountered issues");
                Ok(())
            }
            Err(err) => {
                // Non-OK result indicates a general failure to guarantee
                // all accounts, i.e. we may be disconnected, weren't able to
                // setup a subscription, etc.
                // In that case we don't even want to run the transaction.
                warn!(error = ?err, "Failed to ensure transaction accounts");
                Err(RpcError::transaction_verification(err))
            }
        }
    }
}

/// A prelude module to provide common imports for all RPC handler modules.
mod prelude {
    pub(super) use magicblock_core::Slot;
    pub(super) use solana_account::ReadableAccount;
    pub(super) use solana_account_decoder::{
        UiAccountEncoding, encode_ui_account,
    };
    pub(super) use solana_pubkey::Pubkey;

    pub(super) use super::HandlerResult;
    pub(super) use crate::{
        error::RpcError,
        requests::{
            JsonHttpRequest as JsonRequest,
            params::{Serde32Bytes, SerdeSignature},
            payload::ResponsePayload,
        },
        server::http::dispatch::HttpDispatcher,
        some_or_err,
        utils::{AccountWithPubkey, JsonBody},
    };
}

// --- SPL Token Account Layout Constants ---
// These constants define the data layout of a standard SPL Token account.
const SPL_MINT_OFFSET: usize = 0;
const SPL_OWNER_OFFSET: usize = 32;
const MINT_DECIMALS_OFFSET: usize = 44;
const SPL_TOKEN_AMOUNT_OFFSET: usize = 64;
const SPL_DELEGATE_OFFSET: usize = 76;

const SPL_MINT_RANGE: Range<usize> =
    SPL_MINT_OFFSET..SPL_MINT_OFFSET + size_of::<Pubkey>();
const SPL_TOKEN_AMOUNT_RANGE: Range<usize> =
    SPL_TOKEN_AMOUNT_OFFSET..SPL_TOKEN_AMOUNT_OFFSET + size_of::<u64>();

const TOKEN_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

pub(crate) mod get_account_info;
pub(crate) mod get_balance;
pub(crate) mod get_block;
pub(crate) mod get_block_height;
pub(crate) mod get_block_time;
pub(crate) mod get_blocks;
pub(crate) mod get_blocks_with_limit;
pub(crate) mod get_fee_for_message;
pub(crate) mod get_identity;
pub(crate) mod get_latest_blockhash;
pub(crate) mod get_multiple_accounts;
pub(crate) mod get_program_accounts;
pub(crate) mod get_recent_performance_samples;
pub(crate) mod get_signature_statuses;
pub(crate) mod get_signatures_for_address;
pub(crate) mod get_slot;
pub(crate) mod get_token_account_balance;
pub(crate) mod get_token_accounts_by_delegate;
pub(crate) mod get_token_accounts_by_owner;
pub(crate) mod get_transaction;
pub(crate) mod get_version;
pub(crate) mod is_blockhash_valid;
pub(crate) mod mocked;
pub(crate) mod request_airdrop;
pub(crate) mod send_transaction;
pub(crate) mod simulate_transaction;

// Magic Router compatibility methods.
pub(crate) mod get_delegation_status;
