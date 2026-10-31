use program::SentCommit;
use tracing::{error, info};

use crate::{
    intent_executor::{ExecutionOutput, IntentExecutionReport, error::IntentExecutorResult},
    outbox::ScheduledBaseIntentMeta,
};

pub(crate) fn build_sent_commit(
    meta: ScheduledBaseIntentMeta,
    result: &IntentExecutorResult<ExecutionOutput>,
    execution_report: &IntentExecutionReport,
) -> SentCommit {
    let error_message = result.as_ref().err().map(|err| format!("{:?}", err));

    if let Err(err) = result {
        error!(
            "Failed to commit intent: {}, slot: {}, blockhash: {}. {:?}",
            meta.id, meta.slot, meta.blockhash, err
        );
    }
    let progress = result
        .as_ref()
        .map(|output| output.progress())
        .unwrap_or(execution_report.progress);
    let chain_signatures = progress.signatures(result.as_ref().err()).collect();

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
                error!("Callback failed to schedule: {}. error: {}", meta.id, err);
                format!("ERR: {err}")
            }
        })
        .collect();

    SentCommit {
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
    }
}
