use borsh::BorshDeserialize;
use schedulecommit_program::MainAccount;

use solana_sdk::{pubkey::Pubkey, signature::Signature};

use crate::ScheduleCommitTestContext;

use std::collections::HashMap;

#[derive(Debug, PartialEq, Eq)]
pub struct CommittedAccount {
    pub ephem_account: Option<MainAccount>,
    pub chain_account: Option<MainAccount>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ScheduledCommitResult {
    pub included: HashMap<Pubkey, CommittedAccount>,
    pub excluded: Vec<Pubkey>,
    pub sigs: Vec<Signature>,
}

pub fn fetch_commit_result_from_logs(
    ctx: &ScheduleCommitTestContext,
    sig: Signature,
) -> ScheduledCommitResult {
    // 1. Find scheduled commit sent signature via
    // ScheduledCommitSent signature: <signature>
    let logs = ctx
        .fetch_ephemeral_logs(sig)
        .unwrap_or_else(|| panic!("Logs not found for sig {:?}", sig));
    let scheduled_commmit_send_sig = ctx
        .extract_scheduled_commit_sent_signature(&logs)
        .unwrap_or_else(|| {
            panic!(
                "ScheduledCommitSent signature not found in logs, {:#?}\n{}",
                logs, ctx
            )
        });
    // 2. Find chain commit signature via
    let logs = ctx
        .fetch_ephemeral_logs(scheduled_commmit_send_sig)
        .unwrap_or_else(|| {
            panic!(
                "Logs not found for sig {:?}\n{}",
                scheduled_commmit_send_sig, ctx
            )
        });

    let (included, excluded, sigs) = ctx.extract_sent_commit_info(&logs);

    // 3. Ensure transactions landed on chain
    for sig in &sigs {
        let confirmed = ctx.confirm_transaction_chain(sig).unwrap_or_else(|e| {
            panic!(
                "Transaction with sig {:?} confirmation on chain failed, error: {:?}\n{}",
                sig, e, ctx
            )
        });
        if !confirmed {
            panic!(
                "Transaction {:?} not confirmed on chain within timeout\n{}",
                sig, ctx
            );
        }
    }

    let mut committed_accounts = HashMap::new();
    for pubkey in included {
        let ephem_data = ctx.fetch_ephem_account_data(pubkey).unwrap();
        let ephem_account = if ephem_data.is_empty() {
            None
        } else {
            Some(MainAccount::try_from_slice(&ephem_data).unwrap())
        };
        let chain_data = ctx.fetch_chain_account_data(pubkey).unwrap();
        let chain_account = if chain_data.is_empty() {
            None
        } else {
            Some(MainAccount::try_from_slice(&chain_data).unwrap())
        };
        committed_accounts.insert(
            pubkey,
            CommittedAccount {
                ephem_account,
                chain_account,
            },
        );
    }

    ScheduledCommitResult {
        included: committed_accounts,
        excluded,
        sigs,
    }
}
