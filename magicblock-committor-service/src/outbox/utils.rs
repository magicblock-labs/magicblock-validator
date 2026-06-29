use magicblock_program::{
    magic_scheduled_base_intent::ScheduledIntentBundle, Pubkey, SentCommit,
};
use solana_hash::Hash;
use solana_transaction::Transaction;
use tracing::{error, info};

use crate::{
    intent_executor::{
        error::IntentExecutorResult, ExecutionOutput, IntentExecutionReport,
    },
    outbox::ScheduledBaseIntentMeta,
};

pub(crate) fn build_sent_commit(
    meta: ScheduledBaseIntentMeta,
    result: &IntentExecutorResult<ExecutionOutput>,
    execution_report: &IntentExecutionReport,
) -> (Transaction, SentCommit) {
    let error_message = result.as_ref().err().map(|err| format!("{:?}", err));

    let chain_signatures = match result {
        Ok(output) => match output {
            ExecutionOutput::SingleStage(sig) => vec![*sig],
            ExecutionOutput::TwoStage {
                commit_signature,
                finalize_signature,
            } => vec![*commit_signature, *finalize_signature],
        },
        Err(err) => {
            error!(
                "Failed to commit intent: {}, slot: {}, blockhash: {}. {:?}",
                meta.id, meta.slot, meta.blockhash, err
            );
            err.base_signatures()
                .map(|(commit, finalize)| {
                    finalize.map(|f| vec![commit, f]).unwrap_or(vec![commit])
                })
                .unwrap_or_default()
        }
    };

    let patched_errors = execution_report
        .patched_errors()
        .iter()
        .map(|err| {
            info!("Patched intent: {}. error was: {}", meta.id, err);
            err.to_string()
        })
        .collect();

    let callbacks_scheduling_results = execution_report
        .callbacks_report()
        .iter()
        .map(|r| match r {
            Ok(sig) => format!("OK: {sig}"),
            Err(err) => {
                error!(
                    "Callback failed to schedule: {}. error: {}",
                    meta.id, err
                );
                format!("ERR: {err}")
            }
        })
        .collect();

    let sent_commit = SentCommit {
        message_id: meta.id,
        slot: meta.slot,
        blockhash: meta.blockhash,
        payer: meta.payer,
        chain_signatures,
        included_pubkeys: meta.included_pubkeys,
        excluded_pubkeys: vec![],
        requested_undelegation: meta.requested_undelegation,
        error_message,
        patched_errors,
        callbacks_scheduling_results,
    };

    (meta.intent_sent_transaction, sent_commit)
}
