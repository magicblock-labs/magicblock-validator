use std::ops::Range;

use base64::{prelude::BASE64_STANDARD, Engine};
use http_body_util::BodyExt;
use hyper::{
    body::{Bytes, Incoming},
    Request,
};
use json::Serialize;
use magicblock_gateway_types::accounts::{
    AccountSharedData, AccountsToEnsure, Pubkey,
};
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_status::UiTransactionEncoding;

use crate::{
    error::RpcError, server::http::dispatch::HttpDispatcher,
    state::blocks::BlockHashInfo, RpcResult, Slot,
};

use super::{params::Serde32Bytes, JsonRequest};

pub(crate) enum Data {
    Empty,
    SingleChunk(Bytes),
    MultiChunk(Vec<u8>),
}

pub(crate) fn parse_body(body: Data) -> RpcResult<JsonRequest> {
    let body = match &body {
        Data::Empty => {
            return Err(RpcError::invalid_request("missing request body"));
        }
        Data::SingleChunk(slice) => slice.as_ref(),
        Data::MultiChunk(vec) => vec.as_ref(),
    };
    json::from_slice(body).map_err(Into::into)
}

pub(crate) async fn extract_bytes(
    request: Request<Incoming>,
) -> RpcResult<Data> {
    let mut request = request.into_body();
    let mut data = Data::Empty;
    while let Some(next) = request.frame().await {
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
    }
    Ok(data)
}

impl HttpDispatcher {
    #[inline]
    async fn read_account_with_ensure(
        &self,
        pubkey: &Pubkey,
    ) -> Option<AccountSharedData> {
        let mut ensured = false;
        loop {
            let account = self.accountsdb.get_account(pubkey).ok();
            if account.is_some() || ensured {
                break account;
            }
            let to_ensure = AccountsToEnsure::new(vec![*pubkey]);
            let ready = to_ensure.ready.clone();
            let _ = self.ensure_accounts_tx.send(to_ensure).await;
            ready.notified().await;
            ensured = true;
        }
    }

    fn decode_transaction(
        &self,
        txn: &str,
        encoding: UiTransactionEncoding,
    ) -> RpcResult<VersionedTransaction> {
        let decoded = match encoding {
            UiTransactionEncoding::Base58 => {
                bs58::decode(txn).into_vec().map_err(RpcError::parse_error)
            }
            UiTransactionEncoding::Base64 => {
                BASE64_STANDARD.decode(txn).map_err(RpcError::parse_error)
            }
            _ => {
                return Err(RpcError::invalid_params(
                    "invalid transaction encoding",
                ))
            }
        }?;
        bincode::deserialize(&decoded).map_err(RpcError::invalid_params)
    }
}

const SPL_MINT_OFFSET: usize = 0;
const SPL_OWNER_OFFSET: usize = 32;
const SPL_DECIMALS_OFFSET: usize = 40;
const SPL_DELEGATE_OFFSET: usize = 73;

const SPL_MINT_RANGE: Range<usize> =
    SPL_MINT_OFFSET..SPL_MINT_OFFSET + size_of::<Pubkey>();
const SPL_OWNER_RANGE: Range<usize> =
    SPL_OWNER_OFFSET..SPL_OWNER_OFFSET + size_of::<Pubkey>();
const SPL_TOKEN_AMOUNT_RANGE: Range<usize> =
    SPL_DECIMALS_OFFSET..SPL_DECIMALS_OFFSET + size_of::<u64>();
const SPL_DELEGATE_RANGE: Range<usize> =
    SPL_DELEGATE_OFFSET..SPL_DELEGATE_OFFSET + size_of::<Pubkey>();

const TOKEN_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const TOKEN_2022_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

pub(crate) mod get_account_info;
pub(crate) mod get_balance;
pub(crate) mod get_block;
pub(crate) mod get_block_height;
pub(crate) mod get_block_time;
pub(crate) mod get_blocks;
pub(crate) mod get_blocks_with_limit;
pub(crate) mod get_fees_for_message;
pub(crate) mod get_identity;
pub(crate) mod get_latest_blockhash;
pub(crate) mod get_multiple_accounts;
pub(crate) mod get_program_accounts;
pub(crate) mod get_signature_statuses;
pub(crate) mod get_signatures_for_address;
pub(crate) mod get_slot;
pub(crate) mod get_token_account_balance;
pub(crate) mod get_token_accounts_by_delegate;
pub(crate) mod get_token_accounts_by_owner;
pub(crate) mod get_transaction;
pub(crate) mod is_blockhash_valid;
pub(crate) mod send_transaction;
pub(crate) mod simulate_transaction;
