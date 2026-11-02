//! Failure-only diagnostics for the operator monitor, without replay or delivery guarantees.

use base64::{Engine as _, prelude::BASE64_STANDARD};
use nucleus::runtime::FullTransaction;
use serde::Serialize;
use solana_svm::transaction_processing_result::TransactionProcessingResultExtensions;
use solana_transaction_error::TransactionError;

use super::prelude::*;
use crate::requests::{params::SerdeSignature, payload::NotificationPayload};

/// Diagnostics from one live Engine failure; unavailable recordings serialize as `null`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Failure<'a> {
    signature: SerdeSignature,
    slot: u64,
    error: TransactionError,
    static_account_keys: Vec<Serde32Bytes>,
    resolved_account_keys: Option<Vec<Serde32Bytes>>,
    instructions: Vec<Instruction<'a>>,
    logs: Option<&'a [String]>,
}

/// Compiled instruction with a program address, message account indices, and Base64 data.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Instruction<'a> {
    program_id: Serde32Bytes,
    accounts: &'a [u8],
    data: String,
}

impl<'a> Failure<'a> {
    /// Borrows shared execution recordings and filters successful transactions without consuming them.
    fn new(transaction: &'a FullTransaction) -> Option<Self> {
        let error = transaction.execution.result.flattened_result().err()?;
        let execution = transaction.execution.result.processed_transaction();
        Some(Self {
            signature: SerdeSignature(transaction.transaction.signatures()[0]),
            slot: transaction.execution.slot,
            error,
            static_account_keys: transaction
                .transaction
                .static_account_keys()
                .iter()
                .copied()
                .map(Into::into)
                .collect(),
            // Loading failures may not have resolved lookup-table addresses.
            resolved_account_keys: execution.map(|execution| {
                execution
                    .loaded_transaction
                    .accounts
                    .iter()
                    .map(|(key, _)| (*key).into())
                    .collect()
            }),
            instructions: transaction
                .transaction
                .program_instructions_iter()
                .map(|(program, instruction)| Instruction {
                    program_id: (*program).into(),
                    accounts: instruction.accounts,
                    data: BASE64_STANDARD.encode(instruction.data),
                })
                .collect(),
            logs: execution.and_then(|execution| {
                execution.execution_details.log_messages.as_deref().map(Vec::as_slice)
            }),
        })
    }
}

impl WsDispatcher {
    /// Opens Engine's bounded failure stream without taking the exclusive processed stream.
    /// Slow receivers can close silently; unsubscribe and connection teardown stop forwarding.
    pub(crate) fn transaction_failure_subscribe(&mut self) -> RpcResult<SubResult> {
        let id = next_subid();
        let rx = self.engine.transactions().subscribe_failures();
        self.forward(id, rx, move |transaction| {
            let Some(failure) = Failure::new(&transaction) else { return Ok(None) };
            NotificationPayload::encode_no_context(failure, "transactionFailureNotification", id)
                .map(Some)
        });
        Ok(SubResult::SubId(id))
    }
}
