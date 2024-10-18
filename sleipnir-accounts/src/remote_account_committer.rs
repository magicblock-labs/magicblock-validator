use std::collections::HashSet;

use async_trait::async_trait;
use dlp::instruction::{commit_state, finalize, undelegate, CommitAccountArgs};
use futures_util::future::join_all;
use log::*;
use sleipnir_metrics::metrics;
use sleipnir_program::{validator_authority_id, Pubkey};
use solana_rpc_client::{
    nonblocking::rpc_client::RpcClient, rpc_client::SerializableTransaction,
};
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{
    account::ReadableAccount, clock::MAX_HASH_AGE_IN_SECONDS,
    commitment_config::CommitmentConfig,
    compute_budget::ComputeBudgetInstruction, instruction::Instruction,
    signature::Keypair, signer::Signer, transaction::Transaction,
};

use crate::{
    errors::{AccountsError, AccountsResult},
    AccountCommittee, AccountCommitter, CommitAccountsPayload,
    CommitAccountsTransaction, PendingCommitTransaction,
    SendableCommitAccountsPayload, UndelegationRequest,
};

// [solana_sdk::clock::MAX_HASH_AGE_IN_SECONDS] (120secs) is the max time window at which
// a transaction could still land. For us that is excessive and waiting for 30secs
// should be enough.
const MAX_TRANSACTION_CONFIRMATION_SECS: u64 =
    MAX_HASH_AGE_IN_SECONDS as u64 / 4;

// -----------------
// RemoteAccountCommitter
// -----------------
pub struct RemoteAccountCommitter {
    rpc_client: RpcClient,
    committer_authority: Keypair,
    compute_unit_price: u64,
}

impl RemoteAccountCommitter {
    pub fn new(
        rpc_client: RpcClient,
        committer_authority: Keypair,
        compute_unit_price: u64,
    ) -> Self {
        Self {
            rpc_client,
            committer_authority,
            compute_unit_price,
        }
    }
}

#[async_trait]
impl AccountCommitter for RemoteAccountCommitter {
    async fn create_commit_accounts_transaction(
        &self,
        committees: Vec<AccountCommittee>,
    ) -> AccountsResult<CommitAccountsPayload> {
        // Get blockhash once since this is a slow operation
        let latest_blockhash = self
            .rpc_client
            .get_latest_blockhash()
            .await
            .map_err(|err| {
                AccountsError::FailedToGetLatestBlockhash(err.to_string())
            })?;

        let committee_count: u32 = committees
            .len()
            .try_into()
            .map_err(|_| AccountsError::TooManyCommittees(committees.len()))?;
        let undelegation_count: u32 = committees
            .iter()
            .filter(|c| c.undelegation_request.is_some())
            .count()
            .try_into()
            .map_err(|_| AccountsError::TooManyCommittees(committees.len()))?;
        let (compute_budget_ix, compute_unit_price_ix) =
            self.compute_instructions(committee_count, undelegation_count);

        let mut undelegated_accounts = HashSet::new();
        let mut committed_only_accounts = HashSet::new();
        let mut ixs = vec![compute_budget_ix, compute_unit_price_ix];

        for AccountCommittee {
            pubkey,
            account_data,
            slot,
            undelegation_request,
        } in committees.iter()
        {
            let committer = self.committer_authority.pubkey();
            let commit_args = CommitAccountArgs {
                slot: *slot,
                allow_undelegation: undelegation_request.is_some(),
                data: account_data.data().to_vec(),
            };
            let commit_ix = commit_state(committer, *pubkey, commit_args);

            let finalize_ix = finalize(committer, *pubkey, committer);
            ixs.extend(vec![commit_ix, finalize_ix]);
            if let Some(UndelegationRequest { owner }) = undelegation_request {
                let undelegate_ix = undelegate(
                    validator_authority_id(),
                    *pubkey,
                    *owner,
                    validator_authority_id(),
                );
                ixs.push(undelegate_ix);
                undelegated_accounts.insert(*pubkey);
            } else {
                committed_only_accounts.insert(*pubkey);
            }
        }

        // For now we always commit all accounts in one transaction, but
        // in the future we may split them up into batches to avoid running
        // over the max instruction args size
        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&self.committer_authority.pubkey()),
            &[&self.committer_authority],
            latest_blockhash,
        );
        let committees = committees
            .into_iter()
            .map(|c| (c.pubkey, c.account_data))
            .collect();

        Ok(CommitAccountsPayload {
            transaction: Some(CommitAccountsTransaction {
                transaction: tx,
                undelegated_accounts,
                committed_only_accounts,
            }),
            committees,
        })
    }

    async fn send_commit_transactions(
        &self,
        payloads: Vec<SendableCommitAccountsPayload>,
    ) -> AccountsResult<Vec<PendingCommitTransaction>> {
        let mut pending_commits = Vec::new();
        for SendableCommitAccountsPayload {
            transaction:
                CommitAccountsTransaction {
                    transaction,
                    undelegated_accounts,
                    committed_only_accounts,
                },
            committees,
        } in payloads
        {
            let pubkeys = committees
                .iter()
                .map(|(pubkey, _)| *pubkey)
                .collect::<Vec<_>>();
            let pubkeys_display = pubkeys
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let tx_sig = transaction.get_signature();
            debug!(
                "Committing accounts [{}] sig: {:?} to {}",
                pubkeys_display,
                tx_sig,
                self.rpc_client.url()
            );
            let timer = metrics::account_commit_start();
            let signature = self
                .rpc_client
                .send_transaction_with_config(
                    &transaction,
                    RpcSendTransactionConfig {
                        skip_preflight: true,
                        ..Default::default()
                    },
                )
                .await
                .map_err(|err| {
                    AccountsError::FailedToSendTransaction(err.to_string())
                })?;

            if &signature != tx_sig {
                error!(
                    "Transaction Signature mismatch: {:?} != {:?}",
                    signature, tx_sig
                );
            }
            debug!(
                "Sent commit for [{}] | signature: '{:?}'",
                pubkeys_display, signature
            );
            pending_commits.push(PendingCommitTransaction {
                signature,
                undelegated_accounts,
                committed_only_accounts,
                timer,
            });
        }
        Ok(pending_commits)
    }

    async fn confirm_pending_commits(
        &self,
        pending_commits: Vec<PendingCommitTransaction>,
    ) {
        let mut futures = Vec::new();
        for pc in pending_commits.into_iter() {
            let fut = async move {
                let now = std::time::Instant::now();
                loop {
                    match self
                        .rpc_client
                        .confirm_transaction_with_commitment(
                            &pc.signature,
                            CommitmentConfig::confirmed(),
                        )
                        .await
                    {
                        Ok(res) => {
                            // The RPC `confirm_transaction_with_commitment` doesn't provide
                            // the info to distinguish between a not yet confirmed or
                            // failed transaction.
                            // Failed transactions should be rare, so it's ok to check
                            // them over and over until the timeout is reached.
                            // If we see that happen a lot we can write our custom confirm method
                            // that makes this more straightforward.
                            let confirmed_and_succeeded = res.value;
                            if confirmed_and_succeeded {
                                update_account_commit_metrics(
                                    &pc.undelegated_accounts,
                                    &pc.committed_only_accounts,
                                    metrics::Outcome::from_success(res.value),
                                    Some(pc.timer),
                                );
                                break;
                            } else if now.elapsed().as_secs()
                                > MAX_TRANSACTION_CONFIRMATION_SECS
                            {
                                error!(
                                    "Timed out confirming commit-transaction success '{:?}': {:?}. This means that the transaction failed or failed to confirm in time.",
                                    pc.signature, res
                                );
                                update_account_commit_metrics(
                                    &pc.undelegated_accounts,
                                    &pc.committed_only_accounts,
                                    metrics::Outcome::Error,
                                    None,
                                );
                                break;
                            } else {
                                tokio::time::sleep(
                                    std::time::Duration::from_millis(50),
                                )
                                .await;
                            }
                        }
                        Err(err) => {
                            error!(
                                "Failed to confirm commit transaction '{:?}': {:?}",
                                pc.signature, err
                            );
                            update_account_commit_metrics(
                                &pc.undelegated_accounts,
                                &pc.committed_only_accounts,
                                metrics::Outcome::Error,
                                None,
                            );
                            break;
                        }
                    }
                }

                if log_enabled!(log::Level::Trace) {
                    trace!(
                        "Confirmed commit for {:?} in {:?}",
                        pc.signature,
                        now.elapsed()
                    );
                }
            };
            futures.push(fut);
        }
        join_all(futures).await;
    }
}

pub(crate) fn update_account_commit_metrics(
    commit_and_undelegate_accounts: &HashSet<Pubkey>,
    commit_only_accounts: &HashSet<Pubkey>,
    outcome: metrics::Outcome,
    timer: Option<metrics::HistogramTimer>,
) {
    for pubkey in commit_and_undelegate_accounts {
        metrics::inc_account_commit(
            metrics::AccountCommit::CommitAndUndelegate {
                pubkey: &pubkey.to_string(),
                outcome,
            },
        );
    }
    for pubkey in commit_only_accounts {
        metrics::inc_account_commit(metrics::AccountCommit::CommitOnly {
            pubkey: &pubkey.to_string(),
            outcome,
        });
    }

    // The timer is only present if a transaction's success was confirmed
    if let Some(timer) = timer {
        metrics::account_commit_end(timer);
    }
}

impl RemoteAccountCommitter {
    fn compute_instructions(
        &self,
        committee_count: u32,
        undelegation_count: u32,
    ) -> (Instruction, Instruction) {
        // TODO(thlorenz): We may need to consider account size as well since
        // the account is copied which could affect CUs
        const BASE_COMPUTE_BUDGET: u32 = 50_000;
        const COMPUTE_BUDGET_PER_COMMITTEE: u32 = 30_000;
        const COMPUTE_BUDGET_PER_UNDELEGATION: u32 = 30_000;

        let compute_budget = BASE_COMPUTE_BUDGET
            + (COMPUTE_BUDGET_PER_COMMITTEE * committee_count)
            + (COMPUTE_BUDGET_PER_UNDELEGATION * undelegation_count);

        let compute_budget_ix =
            ComputeBudgetInstruction::set_compute_unit_limit(compute_budget);
        let compute_unit_price_ix =
            ComputeBudgetInstruction::set_compute_unit_price(
                self.compute_unit_price,
            );
        (compute_budget_ix, compute_unit_price_ix)
    }
}
