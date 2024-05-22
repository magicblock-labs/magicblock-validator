use std::str::FromStr;

// NOTE: from rpc/src/rpc.rs :3432
use jsonrpc_core::{futures::future, BoxFuture, Error, Result};
use log::*;
use sleipnir_rpc_client_api::{
    config::{
        RpcBlocksConfigWrapper, RpcContextConfig, RpcEncodingConfigWrapper,
        RpcEpochConfig, RpcRequestAirdropConfig, RpcSendTransactionConfig,
        RpcSignatureStatusConfig, RpcSignaturesForAddressConfig,
        RpcTransactionConfig,
    },
    request::MAX_GET_SIGNATURE_STATUSES_QUERY_ITEMS,
    response::{
        Response as RpcResponse, RpcBlockhash,
        RpcConfirmedTransactionStatusWithSignature, RpcContactInfo,
        RpcInflationReward, RpcPerfSample, RpcPrioritizationFee,
    },
};
use solana_sdk::{
    clock::{Slot, UnixTimestamp, MAX_RECENT_BLOCKHASHES},
    commitment_config::CommitmentConfig,
    hash::Hash,
    message::{SanitizedMessage, SanitizedVersionedMessage, VersionedMessage},
    signature::Signature,
    transaction::VersionedTransaction,
};
use solana_transaction_status::{
    EncodedConfirmedTransactionWithStatusMeta, TransactionBinaryEncoding,
    TransactionStatus, UiTransactionEncoding,
};

use crate::{
    json_rpc_request_processor::JsonRpcRequestProcessor,
    perf::rpc_perf_sample_from,
    traits::rpc_full::Full,
    transaction::{
        decode_and_deserialize, sanitize_transaction, send_transaction,
        verify_signature,
    },
    utils::{new_response, verify_and_parse_signatures_for_address_params},
};

const PERFORMANCE_SAMPLES_LIMIT: usize = 720;

pub struct FullImpl;

#[allow(unused_variables)]
impl Full for FullImpl {
    type Metadata = JsonRpcRequestProcessor;

    fn get_inflation_reward(
        &self,
        meta: Self::Metadata,
        address_strs: Vec<String>,
        config: Option<RpcEpochConfig>,
    ) -> BoxFuture<Result<Vec<Option<RpcInflationReward>>>> {
        todo!("get_inflation_reward")
    }

    fn get_recent_performance_samples(
        &self,
        meta: Self::Metadata,
        limit: Option<usize>,
    ) -> Result<Vec<RpcPerfSample>> {
        debug!("get_recent_performance_samples request received");

        let limit = limit.unwrap_or(PERFORMANCE_SAMPLES_LIMIT);
        if limit > PERFORMANCE_SAMPLES_LIMIT {
            return Err(Error::invalid_params(format!(
                "Invalid limit; max {PERFORMANCE_SAMPLES_LIMIT}"
            )));
        }

        Ok(meta
            .ledger
            .get_recent_perf_samples(limit)
            .map_err(|err| {
                warn!("get_recent_performance_samples failed: {:?}", err);
                Error::invalid_request()
            })?
            .into_iter()
            .map(|(slot, sample)| rpc_perf_sample_from((slot, sample)))
            .collect())
    }

    fn get_cluster_nodes(
        &self,
        meta: Self::Metadata,
    ) -> Result<Vec<RpcContactInfo>> {
        debug!("get_cluster_nodes rpc request received");
        Ok(meta.get_cluster_nodes())
    }

    fn get_signature_statuses(
        &self,
        meta: Self::Metadata,
        signature_strs: Vec<String>,
        config: Option<RpcSignatureStatusConfig>,
    ) -> BoxFuture<Result<RpcResponse<Vec<Option<TransactionStatus>>>>> {
        debug!(
            "get_signature_statuses rpc request received: {:?}",
            signature_strs.len()
        );
        if signature_strs.len() > MAX_GET_SIGNATURE_STATUSES_QUERY_ITEMS {
            return Box::pin(future::err(Error::invalid_params(format!(
                    "Too many inputs provided; max {MAX_GET_SIGNATURE_STATUSES_QUERY_ITEMS}"
                ))));
        }
        let mut signatures: Vec<Signature> = vec![];
        for signature_str in signature_strs {
            match verify_signature(&signature_str) {
                Ok(signature) => {
                    signatures.push(signature);
                }
                Err(err) => return Box::pin(future::err(err)),
            }
        }
        Box::pin(async move {
            meta.get_signature_statuses(signatures, config).await
        })
    }

    fn get_max_retransmit_slot(&self, meta: Self::Metadata) -> Result<Slot> {
        todo!("get_max_retransmit_slot")
    }

    fn get_max_shred_insert_slot(&self, meta: Self::Metadata) -> Result<Slot> {
        todo!("get_max_shred_insert_slot")
    }

    fn request_airdrop(
        &self,
        meta: Self::Metadata,
        pubkey_str: String,
        lamports: u64,
        _config: Option<RpcRequestAirdropConfig>,
    ) -> BoxFuture<Result<String>> {
        debug!("request_airdrop rpc request received");
        Box::pin(
            async move { meta.request_airdrop(pubkey_str, lamports).await },
        )
    }

    fn send_transaction(
        &self,
        meta: Self::Metadata,
        data: String,
        config: Option<RpcSendTransactionConfig>,
    ) -> BoxFuture<Result<String>> {
        debug!("send_transaction rpc request received");
        let RpcSendTransactionConfig {
            skip_preflight,
            preflight_commitment,
            encoding,
            max_retries,
            min_context_slot,
        } = config.unwrap_or_default();

        let tx_encoding = encoding.unwrap_or(UiTransactionEncoding::Base58);

        let preflight_commitment = preflight_commitment
            .map(|commitment| CommitmentConfig { commitment });

        Box::pin(async move {
            send_transaction_impl(
                &meta,
                data,
                preflight_commitment,
                min_context_slot,
                tx_encoding,
                max_retries,
            )
            .await
        })
    }

    fn minimum_ledger_slot(&self, meta: Self::Metadata) -> Result<Slot> {
        todo!("minimum_ledger_slot")
    }

    fn get_block_time(
        &self,
        meta: Self::Metadata,
        slot: Slot,
    ) -> BoxFuture<Result<Option<UnixTimestamp>>> {
        Box::pin(async move { meta.get_block_time(slot).await })
    }

    fn get_transaction(
        &self,
        meta: Self::Metadata,
        signature_str: String,
        config: Option<RpcEncodingConfigWrapper<RpcTransactionConfig>>,
    ) -> BoxFuture<Result<Option<EncodedConfirmedTransactionWithStatusMeta>>>
    {
        debug!("get_transaction rpc request received: {:?}", signature_str);
        let signature = verify_signature(&signature_str);
        if let Err(err) = signature {
            return Box::pin(future::err(err));
        }
        Box::pin(async move {
            meta.get_transaction(signature.unwrap(), config).await
        })
    }

    fn get_blocks(
        &self,
        meta: Self::Metadata,
        start_slot: Slot,
        config: Option<RpcBlocksConfigWrapper>,
        commitment: Option<CommitmentConfig>,
    ) -> BoxFuture<Result<Vec<Slot>>> {
        todo!("get_blocks")
    }

    fn get_blocks_with_limit(
        &self,
        meta: Self::Metadata,
        start_slot: Slot,
        limit: usize,
        commitment: Option<CommitmentConfig>,
    ) -> BoxFuture<Result<Vec<Slot>>> {
        todo!("get_blocks_with_limit")
    }

    fn get_signatures_for_address(
        &self,
        meta: Self::Metadata,
        address: String,
        config: Option<RpcSignaturesForAddressConfig>,
    ) -> BoxFuture<Result<Vec<RpcConfirmedTransactionStatusWithSignature>>>
    {
        let config = config.unwrap_or_default();
        let commitment = config.commitment;

        let verification = verify_and_parse_signatures_for_address_params(
            address,
            config.before,
            config.until,
            config.limit,
        );

        match verification {
            Err(err) => Box::pin(future::err(err)),
            Ok((address, before, until, limit)) => Box::pin(async move {
                meta.get_signatures_for_address(
                    address,
                    before,
                    until,
                    limit,
                    RpcContextConfig {
                        commitment,
                        min_context_slot: None,
                    },
                )
                .await
            }),
        }
    }

    fn get_first_available_block(
        &self,
        meta: Self::Metadata,
    ) -> BoxFuture<Result<Slot>> {
        Box::pin(async move { Ok(meta.get_first_available_block().await) })
    }

    fn get_latest_blockhash(
        &self,
        meta: Self::Metadata,
        _config: Option<RpcContextConfig>,
    ) -> Result<RpcResponse<RpcBlockhash>> {
        debug!("get_latest_blockhash rpc request received");
        meta.get_latest_blockhash()
    }

    fn is_blockhash_valid(
        &self,
        meta: Self::Metadata,
        blockhash: String,
        config: Option<RpcContextConfig>,
    ) -> Result<RpcResponse<bool>> {
        debug!("is_blockhash_valid rpc request received");
        let min_context_slot =
            config.and_then(|config| config.min_context_slot);
        let blockhash = Hash::from_str(&blockhash)
            .map_err(|e| Error::invalid_params(format!("{e:?}")))?;

        meta.is_blockhash_valid(&blockhash, min_context_slot)
    }

    fn get_fee_for_message(
        &self,
        meta: Self::Metadata,
        data: String,
        config: Option<RpcContextConfig>,
    ) -> Result<RpcResponse<Option<u64>>> {
        debug!("get_fee_for_message rpc request received");
        let (_, message) = decode_and_deserialize::<VersionedMessage>(
            data,
            TransactionBinaryEncoding::Base64,
        )?;
        let bank = &*meta.get_bank_with_config(config.unwrap_or_default())?;
        let sanitized_versioned_message =
            SanitizedVersionedMessage::try_from(message).map_err(|err| {
                Error::invalid_params(format!(
                    "invalid transaction message: {err}"
                ))
            })?;
        let sanitized_message =
            SanitizedMessage::try_new(sanitized_versioned_message, bank)
                .map_err(|err| {
                    Error::invalid_params(format!(
                        "invalid transaction message: {err}"
                    ))
                })?;
        let fee = bank.get_fee_for_message(&sanitized_message);
        Ok(new_response(bank, fee))
    }

    fn get_stake_minimum_delegation(
        &self,
        meta: Self::Metadata,
        config: Option<RpcContextConfig>,
    ) -> Result<RpcResponse<u64>> {
        todo!("get_stake_minimum_delegation")
    }

    fn get_recent_prioritization_fees(
        &self,
        meta: Self::Metadata,
        pubkey_strs: Option<Vec<String>>,
    ) -> Result<Vec<RpcPrioritizationFee>> {
        todo!("get_recent_prioritization_fees")
    }
}

async fn send_transaction_impl(
    meta: &JsonRpcRequestProcessor,
    data: String,
    preflight_commitment: Option<CommitmentConfig>,
    min_context_slot: Option<Slot>,
    tx_encoding: UiTransactionEncoding,
    max_retries: Option<usize>,
) -> Result<String> {
    let binary_encoding = tx_encoding.into_binary_encoding().ok_or_else(|| {
                Error::invalid_params(format!(
                    "unsupported encoding: {tx_encoding}. Supported encodings: base58, base64"
                ))
            })?;

    let (_wire_transaction, unsanitized_tx) =
        decode_and_deserialize::<VersionedTransaction>(data, binary_encoding)?;

    let preflight_bank = &*meta.get_bank_with_config(RpcContextConfig {
        commitment: preflight_commitment,
        min_context_slot,
    })?;
    let transaction = sanitize_transaction(unsanitized_tx, preflight_bank)?;
    let signature = *transaction.signature();

    let mut last_valid_block_height = preflight_bank
        .get_blockhash_last_valid_block_height(
            transaction.message().recent_blockhash(),
        )
        .unwrap_or(0);

    let durable_nonce_info = transaction
        .get_durable_nonce()
        .map(|&pubkey| (pubkey, *transaction.message().recent_blockhash()));
    if durable_nonce_info.is_some() {
        // While it uses a defined constant, this last_valid_block_height value is chosen arbitrarily.
        // It provides a fallback timeout for durable-nonce transaction retries in case of
        // malicious packing of the retry queue. Durable-nonce transactions are otherwise
        // retried until the nonce is advanced.
        last_valid_block_height =
            preflight_bank.block_height() + MAX_RECENT_BLOCKHASHES as u64;
    }

    // TODO(thlorenz): leaving out if !skip_preflight part

    send_transaction(
        meta,
        signature,
        transaction,
        last_valid_block_height,
        durable_nonce_info,
        max_retries,
    )
    .await
}
