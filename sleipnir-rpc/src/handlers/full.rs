use std::{cmp::min, str::FromStr};

// NOTE: from rpc/src/rpc.rs :3432
use jsonrpc_core::{futures::future, BoxFuture, Error, Result};
use log::*;
use solana_rpc_client_api::{
    config::{
        RpcBlockConfig, RpcBlocksConfigWrapper, RpcContextConfig,
        RpcEncodingConfigWrapper, RpcEpochConfig, RpcRequestAirdropConfig,
        RpcSendTransactionConfig, RpcSignatureStatusConfig,
        RpcSignaturesForAddressConfig, RpcSimulateTransactionAccountsConfig,
        RpcSimulateTransactionConfig, RpcTransactionConfig,
    },
    request::{
        MAX_GET_CONFIRMED_BLOCKS_RANGE, MAX_GET_SIGNATURE_STATUSES_QUERY_ITEMS,
    },
    response::{
        Response as RpcResponse, RpcBlockhash,
        RpcConfirmedTransactionStatusWithSignature, RpcContactInfo,
        RpcInflationReward, RpcPerfSample, RpcPrioritizationFee,
        RpcSimulateTransactionResult,
    },
};
use solana_sdk::{
    clock::{Slot, UnixTimestamp},
    commitment_config::CommitmentConfig,
    hash::Hash,
    message::{SanitizedMessage, SanitizedVersionedMessage, VersionedMessage},
    signature::Signature,
    transaction::VersionedTransaction,
};
use solana_transaction_status::{
    BlockEncodingOptions, EncodedConfirmedTransactionWithStatusMeta,
    TransactionBinaryEncoding, TransactionStatus, UiConfirmedBlock,
    UiTransactionEncoding,
};

use crate::{
    json_rpc_request_processor::JsonRpcRequestProcessor,
    perf::rpc_perf_sample_from,
    traits::rpc_full::Full,
    transaction::{
        decode_and_deserialize, sanitize_transaction, send_transaction,
        SendTransactionConfig,
    },
    utils::{
        new_response, verify_and_parse_signatures_for_address_params,
        verify_signature,
    },
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
        debug!("get_inflation_reward rpc request received");
        Box::pin(async move {
            Err(Error::invalid_params(
                "Ephemeral validator does not support native staking",
            ))
        })
    }

    fn get_cluster_nodes(
        &self,
        meta: Self::Metadata,
    ) -> Result<Vec<RpcContactInfo>> {
        debug!("get_cluster_nodes rpc request received");
        Ok(meta.get_cluster_nodes())
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
        trace!("signatures: {:?}", signature_strs);
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
        debug!("get_max_retransmit_slot rpc request received");
        Ok(meta.get_bank().slot()) // This doesn't really apply to our validator, but this value is best-effort
    }

    fn get_max_shred_insert_slot(&self, meta: Self::Metadata) -> Result<Slot> {
        debug!("get_max_shred_insert_slot rpc request received");
        Err(Error::invalid_params(
            "Ephemeral validator does not support gossiping of shreds",
        ))
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

    fn simulate_transaction(
        &self,
        meta: Self::Metadata,
        data: String,
        config: Option<RpcSimulateTransactionConfig>,
    ) -> BoxFuture<Result<RpcResponse<RpcSimulateTransactionResult>>> {
        let RpcSimulateTransactionConfig {
            sig_verify,
            replace_recent_blockhash,
            commitment,
            encoding,
            accounts: config_accounts,
            min_context_slot,
            inner_instructions: enable_cpi_recording,
        } = config.unwrap_or_default();
        let tx_encoding = encoding.unwrap_or(UiTransactionEncoding::Base58);

        Box::pin(async move {
            simulate_transaction_impl(
                &meta,
                data,
                tx_encoding,
                config_accounts,
                replace_recent_blockhash,
                sig_verify,
                enable_cpi_recording,
            )
            .await
        })
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
                skip_preflight,
                min_context_slot,
                tx_encoding,
                max_retries,
            )
            .await
        })
    }

    fn minimum_ledger_slot(&self, meta: Self::Metadata) -> Result<Slot> {
        debug!("minimum_ledger_slot rpc request received");
        // We always start the validator on slot 0 and never clear or snapshot the history
        // There will be some related work here: https://github.com/magicblock-labs/magicblock-validator/issues/112
        Ok(0)
    }

    fn get_block(
        &self,
        meta: Self::Metadata,
        slot: Slot,
        config: Option<RpcEncodingConfigWrapper<RpcBlockConfig>>,
    ) -> BoxFuture<Result<Option<UiConfirmedBlock>>> {
        debug!("get_block rpc request received: {}", slot);
        let config = config
            .map(|config| config.convert_to_current())
            .unwrap_or_default();
        let encoding = config.encoding.unwrap_or(UiTransactionEncoding::Json);
        let encoding_options = BlockEncodingOptions {
            transaction_details: config.transaction_details.unwrap_or_default(),
            show_rewards: config.rewards.unwrap_or(true),
            max_supported_transaction_version: config
                .max_supported_transaction_version,
        };
        Box::pin(async move {
            let block = meta.get_block(slot)?;
            let encoded = block
                .map(|block| {
                    block.encode_with_options(encoding, encoding_options)
                })
                .transpose()
                .map_err(|e| Error::invalid_params(format!("{e:?}")))?;
            Ok(encoded)
        })
    }

    fn get_block_time(
        &self,
        meta: Self::Metadata,
        slot: Slot,
    ) -> BoxFuture<Result<Option<UnixTimestamp>>> {
        Box::pin(async move { meta.get_block_time(slot).await })
    }

    fn get_blocks(
        &self,
        meta: Self::Metadata,
        start_slot: Slot,
        config: Option<RpcBlocksConfigWrapper>,
        commitment: Option<CommitmentConfig>,
    ) -> BoxFuture<Result<Vec<Slot>>> {
        let (end_slot, _) =
            config.map(|wrapper| wrapper.unzip()).unwrap_or_default();
        debug!(
            "get_blocks rpc request received: {} -> {:?}",
            start_slot, end_slot
        );
        Box::pin(async move {
            let end_slot = min(
                meta.get_bank().slot().saturating_sub(1),
                end_slot.unwrap_or(u64::MAX),
            );
            if end_slot.saturating_sub(start_slot)
                > MAX_GET_CONFIRMED_BLOCKS_RANGE
            {
                return Err(Error::invalid_params(format!(
                    "Slot range too large; max {MAX_GET_CONFIRMED_BLOCKS_RANGE}"
                )));
            }
            Ok((start_slot..=end_slot).collect())
        })
    }

    fn get_blocks_with_limit(
        &self,
        meta: Self::Metadata,
        start_slot: Slot,
        limit: usize,
        commitment: Option<CommitmentConfig>,
    ) -> BoxFuture<Result<Vec<Slot>>> {
        let limit = u64::try_from(limit).unwrap_or(u64::MAX);
        debug!(
            "get_blocks_with_limit rpc request received: {} (x{:?})",
            start_slot, limit
        );
        Box::pin(async move {
            let end_slot = min(
                meta.get_bank().slot().saturating_sub(1),
                start_slot.saturating_add(limit).saturating_sub(1),
            );
            if end_slot.saturating_sub(start_slot)
                > MAX_GET_CONFIRMED_BLOCKS_RANGE
            {
                return Err(Error::invalid_params(format!(
                    "Slot range too large; max {MAX_GET_CONFIRMED_BLOCKS_RANGE}"
                )));
            }
            Ok((start_slot..=end_slot).collect())
        })
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
        debug!("get_first_available_block rpc request received");
        // In our case, minimum ledger slot is also the oldest slot we can query
        let minimum_ledger_slot = self.minimum_ledger_slot(meta);
        Box::pin(async move { minimum_ledger_slot })
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
        debug!("get_stake_minimum_delegation rpc request received");
        Err(Error::invalid_params(
            "Ephemeral validator does not support native staking",
        ))
    }

    fn get_recent_prioritization_fees(
        &self,
        meta: Self::Metadata,
        pubkey_strs: Option<Vec<String>>,
    ) -> Result<Vec<RpcPrioritizationFee>> {
        let pubkey_strs = pubkey_strs.unwrap_or_default();
        Err(Error::invalid_params(
            "Ephemeral validator does not support or require priority fees",
        ))
    }
}

async fn send_transaction_impl(
    meta: &JsonRpcRequestProcessor,
    data: String,
    preflight_commitment: Option<CommitmentConfig>,
    skip_preflight: bool,
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
            preflight_bank.block_height() + preflight_bank.max_age;
    }

    let preflight_bank = if skip_preflight {
        None
    } else {
        Some(preflight_bank)
    };
    send_transaction(
        meta,
        preflight_bank,
        signature,
        transaction,
        SendTransactionConfig {
            sigverify: !meta.config.disable_sigverify,
            last_valid_block_height,
            durable_nonce_info,
            max_retries,
        },
    )
    .await
}

async fn simulate_transaction_impl(
    meta: &JsonRpcRequestProcessor,
    data: String,
    tx_encoding: UiTransactionEncoding,
    config_accounts: Option<RpcSimulateTransactionAccountsConfig>,
    replace_recent_blockhash: bool,
    sig_verify: bool,
    enable_cpi_recording: bool,
) -> Result<RpcResponse<RpcSimulateTransactionResult>> {
    let binary_encoding = tx_encoding.into_binary_encoding().ok_or_else(|| {
        Error::invalid_params(format!(
            "unsupported encoding: {tx_encoding}. Supported encodings: base58, base64"
        ))
    })?;

    let (_, unsanitized_tx) =
        decode_and_deserialize::<VersionedTransaction>(data, binary_encoding)?;

    meta.simulate_transaction(
        unsanitized_tx,
        config_accounts,
        replace_recent_blockhash,
        sig_verify,
        enable_cpi_recording,
    )
    .await
}
