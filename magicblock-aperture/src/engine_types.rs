//! Conversion from engine ledger types into Solana RPC metadata.

use ledger::{
    request::TransactionResponse,
    schema::{Cpis, Execution, ExecutionDetails},
};
use nucleus::runtime::FullTransaction;
use solana_message::{
    compiled_instruction::CompiledInstruction, v0::LoadedAddresses,
};
use solana_pubkey::Pubkey;
use solana_svm::transaction_processing_result::TransactionProcessingResultExtensions;
use solana_transaction::{
    sanitized::{MessageHash, SanitizedTransaction},
    versioned::VersionedTransaction,
};
use solana_transaction_context::transaction::TransactionReturnData;
use solana_transaction_status::{
    ConfirmedTransactionWithStatusMeta, InnerInstruction, InnerInstructions,
    TransactionStatusMeta, TransactionWithStatusMeta,
    VersionedTransactionWithStatusMeta,
};

use crate::error::RpcError;

/// Converts one retained engine transaction into the canonical Solana status
/// representation shared by transaction and block RPC responses.
pub(crate) fn confirmed_transaction(
    response: TransactionResponse,
    block_time: Option<i64>,
) -> Result<ConfirmedTransactionWithStatusMeta, RpcError> {
    let transaction: VersionedTransaction =
        wincode::deserialize(&response.transaction).map_err(|error| {
            RpcError::internal(format!("invalid engine transaction: {error}"))
        })?;
    let slot = response.execution.header.slot;
    let meta = transaction_meta(response.execution);
    Ok(ConfirmedTransactionWithStatusMeta {
        slot,
        tx_with_meta: TransactionWithStatusMeta::Complete(
            VersionedTransactionWithStatusMeta { transaction, meta },
        ),
        block_time,
        // The engine does not currently persist a transaction's block index.
        index: 0,
    })
}

pub(crate) fn transaction_meta(execution: Execution) -> TransactionStatusMeta {
    let status = execution.header.result;
    let Some(details) = execution.details else {
        return TransactionStatusMeta {
            status,
            ..Default::default()
        };
    };
    meta_from_details(status, details)
}

/// Converts a live processed engine transaction for Geyser delivery.
pub(crate) fn processed_transaction(
    transaction: &FullTransaction,
) -> Result<(SanitizedTransaction, TransactionStatusMeta), RpcError> {
    let versioned: VersionedTransaction = wincode::deserialize(
        transaction.transaction.inner_data(),
    )
    .map_err(|error| {
        RpcError::internal(format!(
            "invalid processed engine transaction: {error}"
        ))
    })?;
    let sanitized = SanitizedTransaction::try_create(
        versioned,
        MessageHash::Compute,
        None,
        solana_message::SimpleAddressLoader::Disabled,
        &Default::default(),
    )
    .map_err(|error| {
        RpcError::internal(format!("invalid processed transaction: {error}"))
    })?;

    let status = transaction.execution.result.flattened_result();
    let Some(execution) = transaction.execution.result.as_ref().ok() else {
        return Ok((
            sanitized,
            TransactionStatusMeta {
                status,
                ..Default::default()
            },
        ));
    };
    let details = &execution.execution_details;
    let (pre_balances, post_balances) = transaction
        .execution
        .balances
        .clone()
        .map(|balances| balances.into_vecs())
        .unwrap_or_default();
    let inner_instructions =
        details.inner_instructions.as_ref().map(|groups| {
            groups
                .iter()
                .enumerate()
                .map(|(index, group)| InnerInstructions {
                    index: u8::try_from(index).unwrap_or(u8::MAX),
                    instructions: group
                        .iter()
                        .map(|instruction| InnerInstruction {
                            instruction: instruction.instruction.clone(),
                            stack_height: Some(instruction.stack_height.into()),
                        })
                        .collect(),
                })
                .collect()
        });
    let meta = TransactionStatusMeta {
        status,
        fee: execution.loaded_transaction.fee_details.total_fee(),
        pre_balances,
        post_balances,
        inner_instructions,
        log_messages: details
            .log_messages
            .as_ref()
            .map(|logs| logs.as_ref().clone()),
        pre_token_balances: None,
        post_token_balances: None,
        rewards: None,
        loaded_addresses: LoadedAddresses::default(),
        return_data: details.return_data.clone(),
        compute_units_consumed: Some(details.executed_units),
        cost_units: None,
    };
    Ok((sanitized, meta))
}

fn meta_from_details(
    status: Result<(), solana_transaction_error::TransactionError>,
    details: ExecutionDetails,
) -> TransactionStatusMeta {
    TransactionStatusMeta {
        status,
        fee: details.fee,
        pre_balances: details.balances.pre,
        post_balances: details.balances.post,
        inner_instructions: details.cpi.map(inner_instructions),
        log_messages: Some(details.logs.as_ref().clone()),
        pre_token_balances: None,
        post_token_balances: None,
        rewards: None,
        loaded_addresses: LoadedAddresses::default(),
        return_data: details.return_data.map(|data| TransactionReturnData {
            program_id: Pubkey::new_from_array(data.program),
            data: data.data.as_ref().clone(),
        }),
        compute_units_consumed: Some(details.compute_units),
        cost_units: None,
    }
}

fn inner_instructions(groups: Vec<Cpis>) -> Vec<InnerInstructions> {
    groups
        .into_iter()
        .enumerate()
        .map(|(index, group)| InnerInstructions {
            index: u8::try_from(index).unwrap_or(u8::MAX),
            instructions: group
                .0
                .into_iter()
                .map(|instruction| InnerInstruction {
                    instruction: CompiledInstruction {
                        program_id_index: instruction.compiled.program_index,
                        accounts: instruction.compiled.accounts,
                        data: instruction.compiled.data,
                    },
                    stack_height: Some(instruction.stack_height.into()),
                })
                .collect(),
        })
        .collect()
}
