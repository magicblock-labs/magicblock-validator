//! Transaction decoding and conversion into Solana RPC metadata.

use base64::{Engine as _, prelude::BASE64_STANDARD};
use nucleus::runtime::{FullTransaction, TransactionView};
use solana_transaction_status::UiTransactionEncoding;
use std::{borrow::Cow, sync::Arc};

use agave_transaction_view::{
    transaction_version::TransactionVersion, transaction_view::UnsanitizedTransactionView,
};
use ledger::{
    request::TransactionResponse,
    schema::{Cpis, Execution},
};
use solana_message::{
    SimpleAddressLoader, compiled_instruction::CompiledInstruction,
    inner_instruction::InnerInstructionsList, v0::LoadedAddresses, v1::V1_PREFIX,
};
use solana_pubkey::Pubkey;
use solana_svm::transaction_processing_result::TransactionProcessingResultExtensions;
use solana_transaction::{
    sanitized::{MessageHash, SanitizedTransaction},
    versioned::VersionedTransaction,
};
use solana_transaction_context::transaction::TransactionReturnData;
use solana_transaction_status::{
    ConfirmedTransactionWithStatusMeta, InnerInstruction, InnerInstructions, TransactionStatusMeta,
    TransactionWithStatusMeta, VersionedTransactionWithStatusMeta,
};

use crate::error::RpcError;

/// Converts one retained engine transaction into the canonical Solana status
/// representation shared by transaction and block RPC responses.
pub(crate) fn confirmed_transaction(
    response: TransactionResponse,
    block_time: Option<i64>,
) -> Result<ConfirmedTransactionWithStatusMeta, RpcError> {
    let transaction = deserialize_transaction(&response.transaction, "invalid engine transaction")?;
    let slot = response.execution.header.slot;
    let meta = transaction_meta(response.execution);
    Ok(ConfirmedTransactionWithStatusMeta {
        slot,
        tx_with_meta: TransactionWithStatusMeta::Complete(VersionedTransactionWithStatusMeta {
            transaction,
            meta,
        }),
        block_time,
        // The engine does not currently persist a transaction's block index.
        index: 0,
    })
}

/// Projects retained Engine metadata without repairing balances or optional fields.
fn transaction_meta(execution: Execution) -> TransactionStatusMeta {
    let status = execution.header.result;
    let Some(details) = execution.details else {
        return TransactionStatusMeta { status, ..Default::default() };
    };
    TransactionStatusMeta {
        status,
        fee: details.fee,
        pre_balances: details.balances.pre,
        post_balances: details.balances.post,
        inner_instructions: details.cpi.map(inner_instructions),
        log_messages: Some(Arc::unwrap_or_clone(details.logs)),
        pre_token_balances: None,
        post_token_balances: None,
        rewards: None,
        loaded_addresses: LoadedAddresses::default(),
        return_data: details.return_data.map(|data| TransactionReturnData {
            program_id: Pubkey::new_from_array(data.program),
            data: Arc::unwrap_or_clone(data.data),
        }),
        compute_units_consumed: Some(details.compute_units),
        cost_units: None,
    }
}

/// Moves live metadata into Geyser output, retaining loaded accounts for account delivery.
pub(crate) fn processed_transaction(
    transaction: &mut FullTransaction,
) -> Result<(SanitizedTransaction, TransactionStatusMeta), RpcError> {
    let versioned = deserialize_transaction(
        transaction.transaction.inner_data(),
        "invalid processed engine transaction",
    )?;
    let sanitized = SanitizedTransaction::try_create(
        versioned,
        MessageHash::Compute,
        None,
        SimpleAddressLoader::Disabled,
        &Default::default(),
    )
    .map_err(|error| RpcError::internal(format!("invalid processed transaction: {error}")))?;

    let status = transaction.execution.result.flattened_result();
    let Some(execution) = transaction.execution.result.as_mut().ok() else {
        return Ok((
            sanitized,
            TransactionStatusMeta { status, ..Default::default() },
        ));
    };
    // Only movable metadata is consumed; loaded account images remain available.
    let details = &mut execution.execution_details;
    let (pre_balances, post_balances) = transaction
        .execution
        .balances
        .take()
        .map(|balances| balances.into_vecs())
        .unwrap_or_default();
    let inner_instructions = details.inner_instructions.take().map(live_inner_instructions);
    let meta = TransactionStatusMeta {
        status,
        fee: execution.loaded_transaction.fee_details.total_fee(),
        pre_balances,
        post_balances,
        inner_instructions,
        log_messages: details.log_messages.take().map(Arc::unwrap_or_clone),
        pre_token_balances: None,
        post_token_balances: None,
        rewards: None,
        loaded_addresses: LoadedAddresses::default(),
        return_data: details.return_data.take(),
        compute_units_consumed: Some(details.executed_units),
        cost_units: None,
    };
    Ok((sanitized, meta))
}

/// Decodes Solana wire bytes, adapting only the Magicblock version prefix.
fn deserialize_transaction(
    bytes: &[u8],
    error_context: &str,
) -> Result<VersionedTransaction, RpcError> {
    let view = UnsanitizedTransactionView::try_new_unsanitized(bytes)
        .map_err(|error| RpcError::internal(format!("{error_context}: {error:?}")))?;
    let bytes = if matches!(view.version(), TransactionVersion::Magicblock) {
        let mut bytes = bytes.to_vec();
        // Solana understands V1 but not Engine's equivalent Magicblock prefix.
        bytes[0] = V1_PREFIX;
        Cow::Owned(bytes)
    } else {
        Cow::Borrowed(bytes)
    };
    wincode::deserialize(&bytes)
        .map_err(|error| RpcError::internal(format!("{error_context}: {error}")))
}

/// Group CPI instructions with the original top-level instruction index.
fn group_inner_instructions(
    groups: impl IntoIterator<Item = impl IntoIterator<Item = InnerInstruction>>,
) -> Vec<InnerInstructions> {
    groups
        .into_iter()
        .enumerate()
        .map(|(index, instructions)| InnerInstructions {
            index: u8::try_from(index).unwrap_or(u8::MAX),
            instructions: instructions.into_iter().collect(),
        })
        .collect()
}

/// Adapts retained CPI records to the shared Solana instruction grouping.
fn inner_instructions(groups: Vec<Cpis>) -> Vec<InnerInstructions> {
    group_inner_instructions(groups.into_iter().map(|group| {
        group.0.into_iter().map(|instruction| InnerInstruction {
            instruction: CompiledInstruction {
                program_id_index: instruction.compiled.program_index,
                accounts: instruction.compiled.accounts,
                data: instruction.compiled.data,
            },
            stack_height: Some(instruction.stack_height.into()),
        })
    }))
}

/// Adapts owned SVM CPI records without an intermediate metadata schema.
pub(crate) fn live_inner_instructions(groups: InnerInstructionsList) -> Vec<InnerInstructions> {
    group_inner_instructions(groups.into_iter().map(|group| {
        group.into_iter().map(|instruction| InnerInstruction {
            instruction: instruction.instruction,
            stack_height: Some(instruction.stack_height.into()),
        })
    }))
}

/// Decodes supported RPC wire encodings and sanitizes the Engine transaction view.
/// Signature verification remains Engine admission's responsibility.
pub(crate) fn decode_transaction(
    transaction: &str,
    encoding: UiTransactionEncoding,
) -> Result<TransactionView, RpcError> {
    let bytes = match encoding {
        UiTransactionEncoding::Base58 => {
            bs58::decode(transaction).into_vec().map_err(RpcError::parse_error)?
        }
        UiTransactionEncoding::Base64 => {
            BASE64_STANDARD.decode(transaction).map_err(RpcError::parse_error)?
        }
        _ => return Err(RpcError::invalid_params("unsupported transaction encoding")),
    };
    TransactionView::try_new_sanitized(Arc::new(bytes), true)
        .map_err(|error| RpcError::invalid_params(format!("{error:?}")))
}
