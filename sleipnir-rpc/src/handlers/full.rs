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
    signature::Signature,
    transaction::VersionedTransaction,
};
use solana_transaction_status::{
    EncodedConfirmedTransactionWithStatusMeta, TransactionStatus,
    UiTransactionEncoding,
};

use crate::{
    json_rpc_request_processor::JsonRpcRequestProcessor,
    traits::rpc_full::Full,
    transaction::{
        decode_and_deserialize, sanitize_transaction, send_transaction,
        verify_signature,
    },
};

// Solana shows the last 60secs worth of samples
const RECENT_PERF_SAMPLES_WINDOW_MILLIS: u32 = 60_000;

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

    fn get_cluster_nodes(
        &self,
        meta: Self::Metadata,
    ) -> Result<Vec<RpcContactInfo>> {
        todo!("get_cluster_nodes")
    }

    fn get_recent_performance_samples(
        &self,
        meta: Self::Metadata,
        limit: Option<usize>,
    ) -> Result<Vec<RpcPerfSample>> {
        debug!("get_recent_performance_samples request received");

        // TODO(thlorenz): we don't have a blockstore, so we just make up some numbers here
        let bank = meta.get_bank()?;
        let num_slots = RECENT_PERF_SAMPLES_WINDOW_MILLIS
            / meta.config.slot_duration.as_millis() as u32;
        let current_slot = bank.slot();
        let min_slot = (current_slot.saturating_sub(num_slots as u64)).max(0);
        let samples = (min_slot..=current_slot)
            .map(|slot| RpcPerfSample {
                slot,
                num_transactions: 0,
                sample_period_secs: (RECENT_PERF_SAMPLES_WINDOW_MILLIS / 1000)
                    as u16,
                num_non_vote_transactions: None,
                num_slots: num_slots as u64,
            })
            .collect();
        Ok(samples)
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
    ) -> Result<String> {
        debug!("request_airdrop rpc request received");
        meta.request_airdrop(pubkey_str, lamports)
    }

    fn send_transaction(
        &self,
        meta: Self::Metadata,
        data: String,
        config: Option<RpcSendTransactionConfig>,
    ) -> Result<String> {
        debug!("send_transaction rpc request received");
        let RpcSendTransactionConfig {
            skip_preflight,
            preflight_commitment,
            encoding,
            max_retries,
            min_context_slot,
        } = config.unwrap_or_default();

        let tx_encoding = encoding.unwrap_or(UiTransactionEncoding::Base58);
        let binary_encoding = tx_encoding.into_binary_encoding().ok_or_else(|| {
                Error::invalid_params(format!(
                    "unsupported encoding: {tx_encoding}. Supported encodings: base58, base64"
                ))
            })?;
        let (wire_transaction, unsanitized_tx) = decode_and_deserialize::<
            VersionedTransaction,
        >(
            data, binary_encoding
        )?;

        let preflight_commitment = preflight_commitment
            .map(|commitment| CommitmentConfig { commitment });
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
            &meta,
            signature,
            // wire_transaction,
            transaction,
            last_valid_block_height,
            durable_nonce_info,
            max_retries,
        )
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
        todo!("get_signatures_for_address")
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
        todo!("is_blockhash_valid")
    }

    fn get_fee_for_message(
        &self,
        meta: Self::Metadata,
        data: String,
        config: Option<RpcContextConfig>,
    ) -> Result<RpcResponse<Option<u64>>> {
        todo!("get_fee_for_message")
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
