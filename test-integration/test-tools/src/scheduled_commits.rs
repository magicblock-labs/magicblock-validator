use std::str::FromStr;

use solana_sdk::{pubkey::Pubkey, signature::Signature};

// -----------------
// Log Extractors
// -----------------
pub fn extract_scheduled_commit_sent_signature_from_logs(
    logs: &[String],
) -> Option<Signature> {
    // ScheduledCommitSent signature: <signature>
    for log in logs {
        if log.starts_with("ScheduledCommitSent signature: ") {
            let commit_sig =
                log.split_whitespace().last().expect("No signature found");
            return Signature::from_str(commit_sig).ok();
        }
    }
    None
}

pub fn extract_sent_commit_info_from_logs(
    logs: &[String],
) -> (Vec<Pubkey>, Vec<Pubkey>, Vec<Signature>) {
    // ScheduledCommitSent included: [6ZQpzi8X2jku3C2ERgZB8hzhQ55VHLm8yZZLwTpMzHw3, 3Q49KuvoEGzGWBsbh2xgrKog66be3UM1aDEsHq7Ym4pr]
    // ScheduledCommitSent excluded: []
    // ScheduledCommitSent signature[0]: g1E7PyWZ3UHFZMJW5KqQsgoZX9PzALh4eekzjg7oGqeDPxEDfipEmV8LtTbb8EbqZfDGEaA9xbd1fADrGDGZZyi
    let mut included = vec![];
    let mut excluded = vec![];
    let mut signgatures = vec![];

    fn pubkeys_from_log_line(log: &str) -> Vec<Pubkey> {
        log.trim_end_matches(']')
            .split_whitespace()
            .skip(2)
            .flat_map(|p| {
                let key = p
                    .trim()
                    .trim_matches(',')
                    .trim_matches('[')
                    .trim_matches(']');
                if key.is_empty() {
                    None
                } else {
                    Pubkey::from_str(key).ok()
                }
            })
            .collect::<Vec<Pubkey>>()
    }

    for log in logs {
        if log.starts_with("ScheduledCommitSent included: ") {
            included = pubkeys_from_log_line(log)
        } else if log.starts_with("ScheduledCommitSent excluded: ") {
            excluded = pubkeys_from_log_line(log)
        } else if log.starts_with("ScheduledCommitSent signature[") {
            let commit_sig = log
                .trim_end_matches(']')
                .split_whitespace()
                .last()
                .and_then(|s| Signature::from_str(s).ok());
            if let Some(commit_sig) = commit_sig {
                signgatures.push(commit_sig);
            }
        }
    }
    (included, excluded, signgatures)
}

pub fn extract_chain_transaction_signature_from_logs(
    logs: &[String],
) -> Option<Signature> {
    for log in logs {
        if log.starts_with("CommitTransactionSignature: ") {
            let commit_sig =
                log.split_whitespace().last().expect("No signature found");
            return Signature::from_str(commit_sig).ok();
        }
    }
    None
}
