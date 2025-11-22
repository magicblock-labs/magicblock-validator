use core::str;
use std::{mem::size_of, ops::Range};

use base64::{prelude::BASE64_STANDARD, Engine};
use http_body_util::BodyExt;
use hyper::{
    body::{Bytes, Incoming},
    Request, Response,
};
use log::*;
use magicblock_core::{
    link::transactions::SanitizeableTransaction, traits::AccountsBank,
};
use magicblock_metrics::metrics::ENSURE_ACCOUNTS_TIME;
use prelude::JsonBody;
use solana_account::AccountSharedData;
use solana_pubkey::Pubkey;
use solana_transaction::{
    sanitized::SanitizedTransaction, versioned::VersionedTransaction,
};
use solana_transaction_status::UiTransactionEncoding;

use super::RpcRequest;
use crate::{
    error::RpcError, server::http::dispatch::HttpDispatcher, RpcResult,
};

pub(crate) type HandlerResult = RpcResult<Response<JsonBody>>;

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
            return Err(RpcError::invalid_request("missing request body"))
        }
        Data::SingleChunk(slice) => slice.as_ref(),
        Data::MultiChunk(vec) => vec.as_ref(),
    };
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
    /// Fetches an account's data from the `AccountsDb` filling it in from chain
    /// as needed.
    async fn read_account_with_ensure(
        &self,
        pubkey: &Pubkey,
    ) -> Option<AccountSharedData> {
        let _timer = ENSURE_ACCOUNTS_TIME
            .with_label_values(&["account"])
            .start_timer();
        let _ = self
            .chainlink
            .ensure_accounts(&[*pubkey], None)
            .await
            .inspect_err(|e| {
                // There is nothing we can do if fetching the account fails
                // Log the error and return whatever is in the accounts db
                debug!("Failed to ensure account {pubkey}: {e}");
            });
        self.accountsdb.get_account(pubkey)
    }

    /// Fetches multiple account's data from the `AccountsDb` filling them in from chain
    /// as needed.
    async fn read_accounts_with_ensure(
        &self,
        pubkeys: &[Pubkey],
    ) -> Vec<Option<AccountSharedData>> {
        trace!("Ensuring accounts {pubkeys:?}");
        let _timer = ENSURE_ACCOUNTS_TIME
            .with_label_values(&["multi-account"])
            .start_timer();
        let _ = self
            .chainlink
            .ensure_accounts(pubkeys, None)
            .await
            .inspect_err(|e| {
                // There is nothing we can do if fetching the accounts fails
                // Log the error and return whatever is in the accounts db
                warn!("Failed to ensure accounts: {e}");
            });
        pubkeys
            .iter()
            .map(|pubkey| self.accountsdb.get_account(pubkey))
            .collect()
    }

    /// Decodes, validates, and sanitizes a transaction from its string representation.
    ///
    /// This is a crucial pre-processing step for both `sendTransaction` and
    /// `simulateTransaction`. It performs the following steps:
    /// 1. Decodes the transaction string using the specified encoding (Base58 or Base64).
    /// 2. Deserializes the binary data into a `VersionedTransaction`.
    /// 3. Validates the transaction's `recent_blockhash` against the ledger, optionally
    ///    replacing it with the latest one.
    /// 4. Sanitizes the transaction, which includes verifying signatures unless disabled.
    fn prepare_transaction(
        &self,
        txn: &str,
        encoding: UiTransactionEncoding,
        sigverify: bool,
        replace_blockhash: bool,
    ) -> RpcResult<SanitizedTransaction> {
        let decoded = match encoding {
            UiTransactionEncoding::Base58 => {
                bs58::decode(txn).into_vec().map_err(RpcError::parse_error)
            }
            UiTransactionEncoding::Base64 => {
                BASE64_STANDARD.decode(txn).map_err(RpcError::parse_error)
            }
            _ => Err(RpcError::invalid_params(
                "unsupported transaction encoding",
            )),
        }?;

        let mut transaction: VersionedTransaction =
            bincode::deserialize(&decoded).map_err(RpcError::invalid_params)?;

        if replace_blockhash {
            transaction
                .message
                .set_recent_blockhash(self.blocks.get_latest().hash);
        } else {
            let hash = transaction.message.recent_blockhash();
            if !self.blocks.contains(hash) {
                return Err(RpcError::transaction_verification(
                    "Blockhash not found",
                ));
            };
        }

        Ok(transaction.sanitize(sigverify)?)
    }

    /// Ensures all accounts required for a transaction are present in the `AccountsDb`.
    async fn ensure_transaction_accounts(
        &self,
        transaction: &SanitizedTransaction,
    ) -> RpcResult<()> {
        let _timer = ENSURE_ACCOUNTS_TIME
            .with_label_values(&["transaction"])
            .start_timer();
        match self
            .chainlink
            .ensure_transaction_accounts(transaction)
            .await
        {
            Ok(res) if res.is_ok() => Ok(()),
            Ok(res) => {
                debug!(
                    "Transaction {} account resolution encountered issues:\n{res}",
                     transaction.signature());
                Ok(())
            }
            Err(err) => {
                // Non-OK result indicates a general failure to guarantee
                // all accounts, i.e. we may be disconnected, weren't able to
                // setup a subscription, etc.
                // In that case we don't even want to run the transaction.
                warn!("Failed to ensure transaction accounts: {:?}", err);
                Err(RpcError::transaction_verification(err))
            }
        }
    }
}

/// A prelude module to provide common imports for all RPC handler modules.
mod prelude {
    pub(super) use magicblock_core::{link::accounts::LockedAccount, Slot};
    pub(super) use solana_account::ReadableAccount;
    pub(super) use solana_account_decoder::UiAccountEncoding;
    pub(super) use solana_pubkey::Pubkey;

    pub(super) use super::HandlerResult;
    pub(super) use crate::{
        error::RpcError,
        requests::{
            params::{Serde32Bytes, SerdeSignature},
            payload::ResponsePayload,
            JsonHttpRequest as JsonRequest,
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
