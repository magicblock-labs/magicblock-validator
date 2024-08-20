use async_trait::async_trait;
use dlp::instruction::{commit_state, finalize, undelegate};
use log::*;
use sleipnir_program::ScheduledCommit;
use solana_rpc_client::{
    nonblocking::rpc_client::RpcClient, rpc_client::SerializableTransaction,
};
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{
    account::ReadableAccount,
    compute_budget::ComputeBudgetInstruction,
    instruction::Instruction,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};

use crate::{
    deleg::CommitAccountArgs,
    errors::{AccountsError, AccountsResult},
    AccountCommittee, AccountCommitter, CommitAccountsPayload,
    SendableCommitAccountsPayload,
};

impl From<(ScheduledCommit, Vec<u8>)> for CommitAccountArgs {
    fn from((commit, data): (ScheduledCommit, Vec<u8>)) -> Self {
        Self {
            slot: commit.slot,
            allow_undelegation: commit.request_undelegation,
            data,
        }
    }
}

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
    async fn create_commit_accounts_transactions(
        &self,
        committees: Vec<AccountCommittee>,
    ) -> AccountsResult<Vec<CommitAccountsPayload>> {
        // Get blockhash once since this is a slow operation
        let latest_blockhash = self
            .rpc_client
            .get_latest_blockhash()
            .await
            .map_err(|err| {
                AccountsError::FailedToGetLatestBlockhash(err.to_string())
            })?;

        let (compute_budget_ix, compute_unit_price_ix) =
            self.compute_instructions();
        let mut ixs = vec![compute_budget_ix, compute_unit_price_ix];

        for AccountCommittee {
            pubkey,
            account_data,
            slot,
            request_undelegation,
        } in committees.iter()
        {
            let committer = self.committer_authority.pubkey();
            let commit_args = CommitAccountArgs {
                slot: *slot,
                allow_undelegation: *request_undelegation,
                data: account_data.data().to_vec(),
            };
            let commit_ix =
                commit_state(committer, *pubkey, commit_args.into_vec());

            let finalize_ix = finalize(committer, *pubkey, committer);
            ixs.extend(vec![commit_ix, finalize_ix]);
            if *request_undelegation {
                // payer: validator_id
                // reimbursement use validator_id pubkey
                // let undelegate_ix = undelegate(*pubkey, committer, vec![]);
                // undelegate(*pubkey, committer, vec![]);
                // TODO(thlorenz): @@@ Need discriminator which probably should get
                // passed when scheduling the commit request
                // let undelegate_ix = allow_undelegate(*pubkey);
                eprintln!("WARN: Pending undelegation");
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

        Ok(vec![CommitAccountsPayload {
            transaction: Some(tx),
            committees,
        }])
    }

    async fn send_commit_transactions(
        &self,
        payloads: Vec<SendableCommitAccountsPayload>,
    ) -> AccountsResult<Vec<Signature>> {
        let mut signatures = Vec::new();
        for SendableCommitAccountsPayload {
            transaction,
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
            signatures.push(signature);
        }
        Ok(signatures)
    }
}

impl RemoteAccountCommitter {
    fn compute_instructions(&self) -> (Instruction, Instruction) {
        // TODO(thlorenz): We may need to compute this budget from the account size since
        // the account is copied which could affect CUs
        const COMPUTE_BUDGET: u32 = 80_000;

        let compute_budget_ix =
            ComputeBudgetInstruction::set_compute_unit_limit(COMPUTE_BUDGET);
        let compute_unit_price_ix =
            ComputeBudgetInstruction::set_compute_unit_price(
                self.compute_unit_price,
            );
        (compute_budget_ix, compute_unit_price_ix)
    }
}
