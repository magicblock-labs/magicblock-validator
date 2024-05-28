use std::any::type_name;

use base64::{prelude::BASE64_STANDARD, Engine};
use bincode::Options;
use jsonrpc_core::{Error, ErrorCode, Result};
use log::*;
use sleipnir_bank::bank::Bank;
use sleipnir_processor::batch_processor::{
    execute_batch, TransactionBatchWithIndexes,
};
use solana_metrics::inc_new_counter_info;
use solana_program_runtime::timings::ExecuteTimings;
use solana_sdk::{
    hash::Hash,
    message::AddressLoader,
    packet::PACKET_DATA_SIZE,
    pubkey::Pubkey,
    signature::Signature,
    system_transaction,
    transaction::{MessageHash, SanitizedTransaction, VersionedTransaction},
};
use solana_transaction_status::TransactionBinaryEncoding;

use crate::json_rpc_request_processor::JsonRpcRequestProcessor;

const MAX_BASE58_SIZE: usize = 1683; // Golden, bump if PACKET_DATA_SIZE changes
const MAX_BASE64_SIZE: usize = 1644; // Golden, bump if PACKET_DATA_SIZE changes

pub(crate) fn decode_and_deserialize<T>(
    encoded: String,
    encoding: TransactionBinaryEncoding,
) -> Result<(Vec<u8>, T)>
where
    T: serde::de::DeserializeOwned,
{
    let wire_output = match encoding {
        TransactionBinaryEncoding::Base58 => {
            inc_new_counter_info!("rpc-base58_encoded_tx", 1);
            if encoded.len() > MAX_BASE58_SIZE {
                return Err(Error::invalid_params(format!(
                    "base58 encoded {} too large: {} bytes (max: encoded/raw {}/{})",
                    type_name::<T>(),
                    encoded.len(),
                    MAX_BASE58_SIZE,
                    PACKET_DATA_SIZE,
                )));
            }
            bs58::decode(encoded).into_vec().map_err(|e| {
                Error::invalid_params(format!("invalid base58 encoding: {e:?}"))
            })?
        }
        TransactionBinaryEncoding::Base64 => {
            inc_new_counter_info!("rpc-base64_encoded_tx", 1);
            if encoded.len() > MAX_BASE64_SIZE {
                return Err(Error::invalid_params(format!(
                    "base64 encoded {} too large: {} bytes (max: encoded/raw {}/{})",
                    type_name::<T>(),
                    encoded.len(),
                    MAX_BASE64_SIZE,
                    PACKET_DATA_SIZE,
                )));
            }
            BASE64_STANDARD.decode(encoded).map_err(|e| {
                Error::invalid_params(format!("invalid base64 encoding: {e:?}"))
            })?
        }
    };
    if wire_output.len() > PACKET_DATA_SIZE {
        return Err(Error::invalid_params(format!(
            "decoded {} too large: {} bytes (max: {} bytes)",
            type_name::<T>(),
            wire_output.len(),
            PACKET_DATA_SIZE
        )));
    }
    bincode::options()
        .with_limit(PACKET_DATA_SIZE as u64)
        .with_fixint_encoding()
        .allow_trailing_bytes()
        .deserialize_from(&wire_output[..])
        .map_err(|err| {
            Error::invalid_params(format!(
                "failed to deserialize {}: {}",
                type_name::<T>(),
                &err.to_string()
            ))
        })
        .map(|output| (wire_output, output))
}

pub(crate) fn sanitize_transaction(
    transaction: VersionedTransaction,
    address_loader: impl AddressLoader,
) -> Result<SanitizedTransaction> {
    SanitizedTransaction::try_create(
        transaction,
        MessageHash::Compute,
        None,
        address_loader,
    )
    .map_err(|err| Error::invalid_params(format!("invalid transaction: {err}")))
}

pub(crate) async fn airdrop_transaction(
    meta: &JsonRpcRequestProcessor,
    pubkey: Pubkey,
    lamports: u64,
) -> Result<String> {
    debug!("request_airdrop rpc request received");
    let bank = meta.get_bank();
    let blockhash = bank.last_blockhash();
    let transaction = system_transaction::transfer(
        &meta.faucet_keypair,
        &pubkey,
        lamports,
        blockhash,
    );

    let transaction =
        SanitizedTransaction::try_from_legacy_transaction(transaction)
            .map_err(|err| {
                Error::invalid_params(format!("invalid transaction: {err}"))
            })?;
    let signature = *transaction.signature();
    send_transaction(meta, None, signature, transaction, 0, None, None).await
}

// TODO(thlorenz): for now we execute the transaction directly via a single batch
pub(crate) async fn send_transaction(
    meta: &JsonRpcRequestProcessor,
    preflight_bank: Option<&Bank>,
    signature: Signature,
    sanitized_transaction: SanitizedTransaction,
    // wire_transaction: Vec<u8>,
    _last_valid_block_height: u64,
    _durable_nonce_info: Option<(Pubkey, Hash)>,
    _max_retries: Option<usize>,
) -> Result<String> {
    let bank = &meta.get_bank();
    // It is very important that we ensure accounts before simulating transactions
    // since they could depend on specific accounts to be in our validator
    meta.accounts_manager
        .ensure_accounts(&sanitized_transaction)
        .await
        .map_err(|err| {
            error!("ensure_accounts failed: {:?}", err);
        })
        .map_err(|err| Error {
            code: ErrorCode::InvalidRequest,
            message: format!("{:?}", err),
            data: None,
        })?;

    if let Some(preflight_bank) = preflight_bank {
        meta.transaction_preflight(preflight_bank, &sanitized_transaction)?;
    }

    // TODO: verify transaction here (sigverify)
    // fn verify_transaction rpc/src/rpc.rs 2002

    let txs = [sanitized_transaction];
    let batch = bank.prepare_sanitized_batch(&txs);
    let batch_with_indexes = TransactionBatchWithIndexes {
        batch,
        // TODO(thlorenz): figure out how to properly derive transaction_indexes
        transaction_indexes: txs
            .iter()
            .enumerate()
            .map(|(idx, _)| idx)
            .collect(),
    };

    let mut timings = ExecuteTimings::default();
    tokio::task::block_in_place(|| {
        execute_batch(
            &batch_with_indexes,
            bank,
            meta.transaction_status_sender(),
            &mut timings,
            None,
        )
        .map_err(|err| jsonrpc_core::Error {
            code: jsonrpc_core::ErrorCode::InternalError,
            message: err.to_string(),
            data: None,
        })
    })?;

    // debug!("{:#?}", tx_result);
    // debug!("{:#?}", tx_balances_set);

    Ok(signature.to_string())
}

pub(crate) fn verify_signature(input: &str) -> Result<Signature> {
    input
        .parse()
        .map_err(|e| Error::invalid_params(format!("Invalid param: {e:?}")))
}
