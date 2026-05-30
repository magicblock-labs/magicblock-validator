use core::str;
use std::{mem::size_of, ops::Range};

use base64::{prelude::BASE64_STANDARD, Engine};
use http_body_util::BodyExt;
use hyper::{
    body::{Bytes, Incoming},
    Request, Response,
};
use magicblock_accounts_db::traits::AccountsBank;
use magicblock_core::{
    coordination_mode::CoordinationMode,
    link::transactions::{SanitizeableTransaction, WithEncoded},
};
use magicblock_metrics::metrics::{AccountFetchOrigin, ENSURE_ACCOUNTS_TIME};
use prelude::JsonBody;
use solana_account::{AccountSharedData, ReadableAccount};
use solana_pubkey::Pubkey;
use solana_transaction::{
    sanitized::SanitizedTransaction, versioned::VersionedTransaction,
};
use solana_transaction_status::UiTransactionEncoding;
use tracing::*;
use transaction_validation::validate_supported_transaction_shape;

use super::RpcRequest;
use crate::{
    error::RpcError, server::http::dispatch::HttpDispatcher, RpcResult,
};

pub(crate) type HandlerResult = RpcResult<Response<JsonBody>>;

const SYSTEM_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("11111111111111111111111111111111");

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
    // Heuristic to render synthetic empty placeholder accounts as JSON-RPC null.
    fn account_should_render_as_null(account: &AccountSharedData) -> bool {
        account.lamports() == 0
            && account.data().is_empty()
            && !account.delegated()
            && !account.undelegating()
            && !account.confined()
            && account.owner() == &SYSTEM_PROGRAM_ID
    }

    fn needs_onchain_interactions(&self) -> bool {
        CoordinationMode::current().needs_onchain_interactions()
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

    /// Fetches an account's data from the `AccountsDb` filling it in from chain
    /// as needed.
    #[instrument(skip_all)]
    async fn read_account_with_ensure(
        &self,
        pubkey: &Pubkey,
    ) -> Option<AccountSharedData> {
        self.read_account_with_ensure_for_user(pubkey, None).await
    }

    /// Like [`read_account_with_ensure`], but additionally ensures the
    /// permission PDA when the request is authenticated — both fetches share a
    /// single chainlink round-trip so the query-filtering path doesn't pay a
    /// second network hop. Pass `None` to skip the permission ensure.
    ///
    /// If the data account is already known to be a permission account
    /// itself, its meta-permission PDA is skipped — permission accounts are
    /// always public and never filtered, so fetching their meta-perm would be
    /// wasted bandwidth.
    #[instrument(skip_all)]
    async fn read_account_with_ensure_for_user(
        &self,
        pubkey: &Pubkey,
        authenticated_user: Option<&Pubkey>,
    ) -> Option<AccountSharedData> {
        if !self.needs_onchain_interactions() {
            return self.accountsdb.get_account(pubkey);
        }

        let mut to_ensure = vec![*pubkey];
        if authenticated_user.is_some() && !self.is_permission_account(pubkey) {
            to_ensure.push(magicblock_query_filtering::types::Permission::pda(
                pubkey,
            ));
        }
        let _timer = ENSURE_ACCOUNTS_TIME
            .with_label_values(&["account"])
            .start_timer();
        let _ = self
            .chainlink
            .ensure_accounts(
                &to_ensure,
                Some(&to_ensure),
                AccountFetchOrigin::GetAccount,
                None,
            )
            .await
            .inspect_err(|e| {
                debug!(error = ?e, "Failed to ensure account");
            });
        self.accountsdb.get_account(pubkey)
    }

    /// Fetches multiple account's data from the `AccountsDb` filling them in from chain
    /// as needed.
    #[instrument(skip(self, pubkeys), fields(pubkey_count = pubkeys.len()))]
    async fn read_accounts_with_ensure(
        &self,
        pubkeys: &[Pubkey],
    ) -> Vec<Option<AccountSharedData>> {
        self.read_accounts_with_ensure_for_user(pubkeys, None).await
    }

    /// Like [`read_accounts_with_ensure`], but bundles each pubkey's
    /// permission PDA into the same chainlink ensure when the request is
    /// authenticated. Single network round-trip for both data and permission
    /// fetches. Permission accounts known locally are skipped — they are
    /// public and never filtered.
    #[instrument(skip(self, pubkeys), fields(pubkey_count = pubkeys.len()))]
    async fn read_accounts_with_ensure_for_user(
        &self,
        pubkeys: &[Pubkey],
        authenticated_user: Option<&Pubkey>,
    ) -> Vec<Option<AccountSharedData>> {
        if !self.needs_onchain_interactions() {
            return pubkeys
                .iter()
                .map(|pubkey| self.accountsdb.get_account(pubkey))
                .collect();
        }

        let mut to_ensure: Vec<Pubkey> = pubkeys.to_vec();
        if authenticated_user.is_some() {
            to_ensure.extend(pubkeys.iter().filter_map(|pubkey| {
                (!self.is_permission_account(pubkey)).then(|| {
                    magicblock_query_filtering::types::Permission::pda(pubkey)
                })
            }));
        }
        trace!("Ensuring accounts");
        let _timer = ENSURE_ACCOUNTS_TIME
            .with_label_values(&["multi-account"])
            .start_timer();
        let _ = self
            .chainlink
            .ensure_accounts(
                &to_ensure,
                Some(&to_ensure),
                AccountFetchOrigin::GetMultipleAccounts,
                None,
            )
            .await
            .inspect_err(|e| {
                warn!(error = ?e, "Failed to ensure accounts");
            });
        pubkeys
            .iter()
            .map(|pubkey| self.accountsdb.get_account(pubkey))
            .collect()
    }

    /// Cheap local check: does the address already resolve to an account
    /// owned by the permission program? Used to short-circuit meta-permission
    /// ensures for accounts we know to be permission accounts themselves.
    fn is_permission_account(&self, pubkey: &Pubkey) -> bool {
        self.accountsdb.get_account(pubkey).is_some_and(|account| {
            account.owner()
                == &magicblock_query_filtering::PERMISSION_PROGRAM_ID
        })
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
    ///
    /// Returns `WithEncoded<SanitizedTransaction>` with the original wire bytes.
    /// For execution (replace_blockhash=false), bytes are preserved for replication.
    /// For simulation (replace_blockhash=true), bytes are unused.
    fn prepare_transaction(
        &self,
        txn: &str,
        encoding: UiTransactionEncoding,
        sigverify: bool,
        replace_blockhash: bool,
    ) -> RpcResult<WithEncoded<SanitizedTransaction>> {
        // parse the string as bincode serialized bytes
        let encoded = match encoding {
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
            bincode::deserialize(&encoded).map_err(RpcError::invalid_params)?;

        validate_supported_transaction_shape(&transaction)?;

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

        let txn = transaction.sanitize(sigverify)?;
        Ok(WithEncoded {
            txn,
            encoded: encoded.into(),
        })
    }

    /// Enforces send/simulate transaction admission against query-filtering
    /// permissions when the caller is authenticated. No-op when query
    /// filtering is disabled or the request has no authenticated user.
    pub(crate) async fn enforce_transaction_admission(
        &self,
        transaction: &SanitizedTransaction,
        request: &super::JsonHttpRequest,
    ) -> RpcResult<()> {
        if request.authenticated_user.is_none() {
            return Ok(());
        }
        let message = transaction.message();
        let account_keys: Vec<Pubkey> =
            message.account_keys().iter().copied().collect();
        self.ensure_permission_accounts(
            &account_keys,
            AccountFetchOrigin::SendTransaction(*transaction.signature()),
        )
        .await;
        magicblock_query_filtering::check_transaction_admission(
            &*self.accountsdb,
            &account_keys,
            message.instructions(),
        )
        .map_err(|err| match err {
            magicblock_query_filtering::service::QueryFilteringError::AccessDenied => {
                RpcError::invalid_request(err)
            }
            other => RpcError::internal(other),
        })
    }

    /// Ensures the permission PDAs for the given data accounts are present in
    /// the local `AccountsDb`. Without this step the query-filtering layer
    /// would treat a not-yet-cloned permission account as missing, silently
    /// rendering a restricted account as unrestricted on first access.
    ///
    /// Accounts already known locally to be permission accounts themselves
    /// are skipped — permission accounts are public and never filtered, so
    /// fetching their meta-permission would be wasted bandwidth.
    ///
    /// `mark_empty_if_not_found` is set so PDAs that don't exist on chain are
    /// cached as empty locally — subsequent reads short-circuit through the
    /// `permission_for_account` owner gate without re-fetching.
    pub(crate) async fn ensure_permission_accounts(
        &self,
        accounts: &[Pubkey],
        fetch_origin: AccountFetchOrigin,
    ) {
        if !self.needs_onchain_interactions() || accounts.is_empty() {
            return;
        }
        let permission_pdas: Vec<Pubkey> = accounts
            .iter()
            .filter(|pubkey| !self.is_permission_account(pubkey))
            .map(magicblock_query_filtering::types::Permission::pda)
            .collect();
        if permission_pdas.is_empty() {
            return;
        }
        let _timer = ENSURE_ACCOUNTS_TIME
            .with_label_values(&["permission"])
            .start_timer();
        let _ = self
            .chainlink
            .ensure_accounts(
                &permission_pdas,
                Some(&permission_pdas),
                fetch_origin,
                None,
            )
            .await
            .inspect_err(|err| {
                warn!(error = ?err, "Failed to ensure permission accounts");
            });
    }

    /// Ensures all accounts required for a transaction are present in the `AccountsDb`.
    #[instrument(skip_all)]
    async fn ensure_transaction_accounts(
        &self,
        transaction: &SanitizedTransaction,
    ) -> RpcResult<()> {
        self.require_primary_rpc_method("transaction")?;

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
mod transaction_validation;

// Magic Router compatibility methods.
pub(crate) mod get_delegation_status;
