use std::ops::Range;

use base64::{prelude::BASE64_STANDARD, Engine};
use http_body_util::BodyExt;
use hyper::{
    body::{Bytes, Incoming},
    Request, Response,
};
use magicblock_core::link::{
    blocks::BlockHash, transactions::SanitizeableTransaction,
};
use prelude::JsonBody;
use solana_account::AccountSharedData;
use solana_message::SimpleAddressLoader;
use solana_pubkey::Pubkey;
use solana_transaction::{
    sanitized::{MessageHash, SanitizedTransaction},
    versioned::VersionedTransaction,
};
use solana_transaction_status::UiTransactionEncoding;

use crate::{
    error::RpcError, server::http::dispatch::HttpDispatcher, RpcResult,
};

use super::JsonHttpRequest;

pub(crate) type HandlerResult = RpcResult<Response<JsonBody>>;

pub(crate) enum Data {
    Empty,
    SingleChunk(Bytes),
    MultiChunk(Vec<u8>),
}

pub(crate) fn parse_body(body: Data) -> RpcResult<JsonHttpRequest> {
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
    async fn read_account_with_ensure(
        &self,
        pubkey: &Pubkey,
    ) -> Option<AccountSharedData> {
        // TODO(thlorenz): use chainlink
        self.accountsdb.get_account(pubkey)
    }

    fn prepare_transaction(
        &self,
        txn: &str,
        encoding: UiTransactionEncoding,
        sigverify: bool,
        replace_blockhash: bool,
    ) -> RpcResult<SanitizedTransaction> {
        // decode the transaction from string using specified encoding
        let decoded = match encoding {
            UiTransactionEncoding::Base58 => {
                bs58::decode(txn).into_vec().map_err(RpcError::parse_error)
            }
            UiTransactionEncoding::Base64 => {
                BASE64_STANDARD.decode(txn).map_err(RpcError::parse_error)
            }
            _ => Err(RpcError::invalid_params("unknown transaction encoding"))?,
        }?;
        // deserialize the transaction from bincode format
        // NOTE: Transaction (legacy) can be directly deserialized into
        // VersionedTransaction due to the compatible binary ABI
        let mut transaction: VersionedTransaction =
            bincode::deserialize(&decoded).map_err(RpcError::invalid_params)?;
        // Verify that the transaction uses valid recent blockhash
        if !replace_blockhash {
            let hash = transaction.message.recent_blockhash();
            self.blocks.get(&hash).ok_or_else(|| {
                RpcError::transaction_verification("Blockhash not found")
            })?;
        } else {
            transaction
                .message
                .set_recent_blockhash(self.blocks.get_latest().hash);
        }
        // sanitize the transaction making it processable
        let transaction = if sigverify {
            transaction.sanitize().map_err(RpcError::invalid_params)?
        } else {
            // for transaction simulation we bypass signature verification entirely
            SanitizedTransaction::try_create(
                transaction,
                MessageHash::Precomputed(BlockHash::new_unique()),
                Some(false),
                SimpleAddressLoader::Disabled,
                &Default::default(),
            )?
        };
        Ok(transaction)
    }

    async fn ensure_transaction_accounts(
        &self,
        transaction: &SanitizedTransaction,
    ) -> RpcResult<()> {
        // TODO(thlorenz): replace the entire method call with chainlink
        let message = transaction.message();
        let reader = self.accountsdb.reader().map_err(RpcError::internal)?;
        let accounts = message.account_keys().iter();
        for pubkey in accounts {
            if !reader.contains(pubkey) {
                panic!("account is not present in the database: {pubkey}");
            }
        }
        Ok(())
    }
}

mod prelude {
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
    pub(super) use magicblock_core::{link::accounts::LockedAccount, Slot};
    pub(super) use solana_account::ReadableAccount;
    pub(super) use solana_account_decoder::UiAccountEncoding;
    pub(super) use solana_pubkey::Pubkey;
}

const SPL_MINT_OFFSET: usize = 0;
const SPL_OWNER_OFFSET: usize = 32;
const SPL_DECIMALS_OFFSET: usize = 40;
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
