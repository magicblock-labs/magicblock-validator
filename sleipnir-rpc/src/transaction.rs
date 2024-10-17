use std::any::type_name;

use base64::{prelude::BASE64_STANDARD, Engine};
use bincode::Options;
use jsonrpc_core::{Error, ErrorCode, Result};
use log::*;
use sleipnir_accounts::{errors::AccountsResult, AccountsManager};
use sleipnir_bank::bank::Bank;
use sleipnir_metrics::metrics;
use sleipnir_processor::execute_transaction::execute_sanitized_transaction;
use solana_metrics::inc_new_counter_info;
use solana_rpc_client_api::custom_error::RpcCustomError;
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
    sigverify: bool,
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
    send_transaction(
        meta,
        None,
        signature,
        transaction,
        SendTransactionConfig {
            sigverify,
            last_valid_block_height: 0,
            durable_nonce_info: None,
            max_retries: None,
        },
    )
    .await
}

pub(crate) struct SendTransactionConfig {
    pub sigverify: bool,
    // pub wire_transaction: Vec<u8>,
    #[allow(unused)]
    pub last_valid_block_height: u64,
    #[allow(unused)]
    pub durable_nonce_info: Option<(Pubkey, Hash)>,
    #[allow(unused)]
    pub max_retries: Option<usize>,
}

// TODO(thlorenz): for now we execute the transaction directly via a single batch
pub(crate) async fn send_transaction(
    meta: &JsonRpcRequestProcessor,
    preflight_bank: Option<&Bank>,
    signature: Signature,
    sanitized_transaction: SanitizedTransaction,
    config: SendTransactionConfig,
) -> Result<String> {
    let SendTransactionConfig { sigverify, .. } = config;
    let bank = &meta.get_bank();

    if sigverify {
        metrics::observe_sigverify_time(|| {
            sig_verify_transaction(&sanitized_transaction)
        })?;
    }

    // It is very important that we ensure accounts before simulating transactions
    // since they could depend on specific accounts to be in our validator
    ensure_accounts(&meta.accounts_manager, &sanitized_transaction)
        .await
        .map_err(|err| Error {
            code: ErrorCode::InvalidRequest,
            message: format!("{:?}", err),
            data: None,
        })?;

    if let Some(preflight_bank) = preflight_bank {
        meta.transaction_preflight(preflight_bank, &sanitized_transaction)?;
    }

    execute_sanitized_transaction(
        sanitized_transaction,
        bank,
        meta.transaction_status_sender(),
    )
    .map_err(|err| jsonrpc_core::Error {
        code: jsonrpc_core::ErrorCode::InternalError,
        message: err.to_string(),
        data: None,
    })?;

    // debug!("{:#?}", tx_result);
    // debug!("{:#?}", tx_balances_set);

    Ok(signature.to_string())
}

/// Verifies only the transaction signature and is used when sending a
/// transaction to avoid the extra overhead of [sig_verify_transaction_and_check_precompiles]
/// TODO(thlorenz): @@ sigverify takes upwards of 90µs which is 30%+ of
/// the entire time it takes to execute a transaction.
/// Therefore this an intermediate solution and we need to investigate verifying the
/// wire_transaction instead (solana sigverify implementation is packet based)
pub(crate) fn sig_verify_transaction(
    transaction: &SanitizedTransaction,
) -> Result<()> {
    let now = match log::log_enabled!(log::Level::Trace) {
        true => Some(std::time::Instant::now()),
        false => None,
    };
    #[allow(clippy::question_mark)]
    if transaction.verify().is_err() {
        return Err(
            RpcCustomError::TransactionSignatureVerificationFailure.into()
        );
    }
    if let Some(now) = now {
        trace!("Sigverify took: {:?}", now.elapsed());
    }

    Ok(())
}

/// Verifies both transaction signature and precompiles which results in
/// max overhead and thus should only be used when simulating transactions
// pub(crate) fn sig_verify_transaction_and_check_precompiles(
//     transaction: &SanitizedTransaction,
//     feature_set: &feature_set::FeatureSet,
// ) -> Result<()> {
//     sig_verify_transaction(transaction)?;
//
//     #[allow(clippy::question_mark)]
//     if transaction.verify().is_err() {
//         return Err(
//             RpcCustomError::TransactionSignatureVerificationFailure.into()
//         );
//     }
//
//     if let Err(e) = transaction.verify_precompiles(feature_set) {
//         return Err(RpcCustomError::TransactionPrecompileVerificationFailure(
//             e,
//         )
//         .into());
//     }
//
//     Ok(())
// }

pub(crate) async fn ensure_accounts(
    accounts_manager: &AccountsManager,
    sanitized_transaction: &SanitizedTransaction,
) -> AccountsResult<Vec<Signature>> {
    accounts_manager
        .ensure_accounts(sanitized_transaction)
        .await
        .map_err(|err| {
            error!("ensure_accounts failed: {:?}", err);
            err
        })
}
