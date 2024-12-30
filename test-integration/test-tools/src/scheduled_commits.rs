use anyhow::{bail, Context, Result};
use borsh::BorshDeserialize;
use solana_sdk::{pubkey::Pubkey, signature::Signature};
use std::collections::HashSet;
use std::str::FromStr;
use std::{collections::HashMap, fmt};

use crate::IntegrationTestContext;

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
) -> (
    Vec<Pubkey>,
    Vec<Pubkey>,
    HashSet<(Pubkey, Pubkey)>,
    Vec<Signature>,
) {
    // ScheduledCommitSent included: [6ZQpzi8X2jku3C2ERgZB8hzhQ55VHLm8yZZLwTpMzHw3, 3Q49KuvoEGzGWBsbh2xgrKog66be3UM1aDEsHq7Ym4pr]
    // ScheduledCommitSent excluded: []
    // ScheduledCommitSent fee payers: [GGFXZZbScG1bVNfjgvAGwr3wSgKxNjL7t1AZSmZSnfyk : EJRyerukCkmNFxowv9ms3TugNkUS5Rgs3kMM7aqmQjQh]
    // ScheduledCommitSent signature[0]: g1E7PyWZ3UHFZMJW5KqQsgoZX9PzALh4eekzjg7oGqeDPxEDfipEmV8LtTbb8EbqZfDGEaA9xbd1fADrGDGZZyi
    let mut included = vec![];
    let mut excluded = vec![];
    let mut feepayers: HashSet<(Pubkey, Pubkey)> = HashSet::new();
    let mut signatures = vec![];

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

    fn pubkey_owner_tuple_hashset_from_log_line(log: &str) -> HashSet<(Pubkey, Pubkey)> {
        log.trim_end_matches(']')
            .split_whitespace()
            .skip(3)
            .flat_map(|p| {
                let parts: Vec<&str> = p
                    .trim()
                    .trim_matches(',')
                    .trim_matches('[')
                    .trim_matches(']')
                    .split(':')
                    .map(|s| s.trim())
                    .collect();
                if parts.len() == 2 {
                    match (
                        Pubkey::from_str(parts[0]),
                        Pubkey::from_str(parts[1]),
                    ) {
                        (Ok(key1), Ok(key2)) => Some((key1, key2)),
                        _ => None,
                    }
                } else {
                    None
                }
            })
            .collect::<HashSet<(Pubkey, Pubkey)>>()
    }

    for log in logs {
        if log.starts_with("ScheduledCommitSent included: ") {
            included = pubkeys_from_log_line(log)
        } else if log.starts_with("ScheduledCommitSent excluded: ") {
            excluded = pubkeys_from_log_line(log)
        } else if log.starts_with("ScheduledCommitSent fee payers: ") {
            feepayers = pubkey_owner_tuple_hashset_from_log_line(log)
        } else if log.starts_with("ScheduledCommitSent signature[") {
            let commit_sig = log
                .trim_end_matches(']')
                .split_whitespace()
                .last()
                .and_then(|s| Signature::from_str(s).ok());
            if let Some(commit_sig) = commit_sig {
                signatures.push(commit_sig);
            }
        }
    }
    (included, excluded, feepayers, signatures)
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

// -----------------
// Fetch Commit Results
// -----------------
#[derive(Debug, PartialEq, Eq)]
pub struct ScheduledCommitResult<T>
where
    T: fmt::Debug + BorshDeserialize + PartialEq + Eq,
{
    pub included: HashMap<Pubkey, T>,
    pub excluded: Vec<Pubkey>,
    pub feepayers: HashSet<(Pubkey, Pubkey)>,
    pub sigs: Vec<Signature>,
}

impl<T> ScheduledCommitResult<T>
where
    T: fmt::Debug + BorshDeserialize + PartialEq + Eq,
{
    pub fn confirm_commit_transactions_on_chain(
        &self,
        ctx: &IntegrationTestContext,
    ) -> Result<()> {
        for sig in &self.sigs {
            let confirmed =
                ctx.confirm_transaction_chain(sig).with_context(|| {
                    format!(
                        "Transaction with sig {:?} confirmation on chain failed",
                        sig
                    )
                })?;
            if !confirmed {
                bail!(
                    "Transaction {:?} not confirmed on chain within timeout",
                    sig
                );
            }
        }
        Ok(())
    }
}

impl IntegrationTestContext {
    pub fn fetch_schedule_commit_result<T>(
        &self,
        sig: Signature,
    ) -> Result<ScheduledCommitResult<T>>
    where
        T: fmt::Debug + BorshDeserialize + PartialEq + Eq,
    {
        // 1. Find scheduled commit sent signature via
        // ScheduledCommitSent signature: <signature>
        let (ephem_logs, scheduled_commmit_sent_sig) = {
            let logs = self.fetch_ephemeral_logs(sig).with_context(|| {
                format!(
                    "Scheduled commit sent logs not found for sig {:?}",
                    sig
                )
            })?;
            let sig =
                extract_scheduled_commit_sent_signature_from_logs(&logs)
                    .with_context(|| {
                        format!("ScheduledCommitSent signature not found in logs, {:#?}", logs)
                    })?;

            (logs, sig)
        };

        // 2. Find chain commit signatures
        let chain_logs = self
            .fetch_ephemeral_logs(scheduled_commmit_sent_sig)
            .with_context(|| {
                format!(
                    "Logs {:#?}\nScheduled commit sent sig {:?}",
                    ephem_logs, scheduled_commmit_sent_sig
                )
            })?;

        let (included, excluded, feepayers, sigs) =
            extract_sent_commit_info_from_logs(&chain_logs);

        let mut committed_accounts = HashMap::new();
        for pubkey in included {
            if feepayers.iter().map(|(_, p)| p).any(|p| p == &pubkey) {
                continue;
            }
            let ephem_data = self.fetch_ephem_account_data(pubkey)?;
            if !ephem_data.is_empty() {
                let ephem_account = T::try_from_slice(&ephem_data)
                    .with_context(|| {
                        format!(
                        "Failed to deserialize ephemeral account data for {:?}",
                        pubkey
                    )
                    })?;
                committed_accounts.insert(pubkey, ephem_account);
            };
        }

        Ok(ScheduledCommitResult {
            included: committed_accounts,
            excluded,
            feepayers,
            sigs,
        })
    }
}
