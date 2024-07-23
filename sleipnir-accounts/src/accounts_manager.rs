use std::sync::Arc;

use conjunto_transwise::{
    RpcProviderConfig, TransactionAccountsExtractorImpl, Transwise,
};
use log::*;
use sleipnir_account_updates::RemoteAccountUpdates;
use sleipnir_bank::bank::Bank;
use sleipnir_program::{
    commit_sender::{
        TriggerCommitCallback, TriggerCommitOutcome, TriggerCommitReceiver,
    },
    errors::{MagicError, MagicErrorWithContext},
};
use sleipnir_transaction_status::TransactionStatusSender;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
};

use crate::{
    bank_account_provider::BankAccountProvider,
    config::AccountsConfig,
    errors::{AccountsError, AccountsResult},
    external_accounts::{ExternalReadonlyAccounts, ExternalWritableAccounts},
    remote_account_cloner::RemoteAccountCloner,
    remote_account_committer::RemoteAccountCommitter,
    utils::{get_epoch, try_rpc_cluster_from_cluster},
    ExternalAccountsManager,
};

pub type AccountsManager = ExternalAccountsManager<
    BankAccountProvider,
    RemoteAccountCloner,
    RemoteAccountCommitter,
    RemoteAccountUpdates,
    Transwise,
    TransactionAccountsExtractorImpl,
>;

impl
    ExternalAccountsManager<
        BankAccountProvider,
        RemoteAccountCloner,
        RemoteAccountCommitter,
        RemoteAccountUpdates,
        Transwise,
        TransactionAccountsExtractorImpl,
    >
{
    pub fn try_new(
        bank: &Arc<Bank>,
        account_updates: RemoteAccountUpdates,
        transaction_status_sender: Option<TransactionStatusSender>,
        committer_authority: Keypair,
        config: AccountsConfig,
    ) -> AccountsResult<Self> {
        let external_config = config.external;
        let cluster = external_config.cluster;
        let internal_account_provider = BankAccountProvider::new(bank.clone());
        let rpc_cluster = try_rpc_cluster_from_cluster(&cluster)?;
        let rpc_client = RpcClient::new_with_commitment(
            rpc_cluster.url().to_string(),
            CommitmentConfig::confirmed(),
        );
        let rpc_provider_config = RpcProviderConfig::new(rpc_cluster, None);

        let account_cloner = RemoteAccountCloner::new(
            cluster,
            bank.clone(),
            transaction_status_sender,
        );
        let account_committer = RemoteAccountCommitter::new(
            rpc_client,
            committer_authority,
            config.commit_compute_unit_price,
        );

        let validated_accounts_provider = Transwise::new(rpc_provider_config);

        Ok(Self {
            internal_account_provider,
            account_cloner,
            account_committer,
            account_updates,
            validated_accounts_provider,
            transaction_accounts_extractor: TransactionAccountsExtractorImpl,
            external_readonly_accounts: ExternalReadonlyAccounts::default(),
            external_writable_accounts: ExternalWritableAccounts::default(),
            external_readonly_mode: external_config.readonly,
            external_writable_mode: external_config.writable,
            create_accounts: config.create,
            payer_init_lamports: config.payer_init_lamports,
        })
    }

    pub fn install_manual_commit_trigger(
        manager: &Arc<Self>,
        rcvr: TriggerCommitReceiver,
    ) {
        fn communicate_trigger_success(
            tx: TriggerCommitCallback,
            outcome: TriggerCommitOutcome,
            pubkey: &Pubkey,
            sig: Option<&Signature>,
        ) {
            if tx.send(Ok(outcome)).is_err() {
                error!(
                    "Failed to ack trigger to commit '{}' with signature '{:?}'",
                    pubkey, sig
                );
            } else {
                debug!(
                    "Acked trigger to commit '{}' with signature '{:?}'",
                    pubkey, sig
                );
            }
        }

        let manager = manager.clone();
        tokio::spawn(async move {
            while let Ok((pubkey, tx)) = rcvr.recv() {
                let now = get_epoch();
                let (commit_infos, signatures) = match manager
                    .create_transactions_to_commit_specific_accounts(vec![
                        pubkey,
                    ])
                    .await
                {
                    Ok(commit_infos) => {
                        let sigs = commit_infos
                            .iter()
                            .flat_map(|(k, v)| {
                                v.signature().map(|sig| (*k, sig))
                            })
                            .collect::<Vec<_>>();
                        (commit_infos, sigs)
                    }
                    Err(ref err) => {
                        use AccountsError::*;
                        let context = match err {
                            InvalidRpcUrl(msg)
                            | FailedToGetLatestBlockhash(msg)
                            | FailedToSendTransaction(msg)
                            | FailedToConfirmTransaction(msg) => {
                                format!("{} ({:?})", msg, err)
                            }
                            _ => format!("{:?}", err),
                        };
                        if tx
                            .send(Err(MagicErrorWithContext::new(
                                MagicError::InternalError,
                                context,
                            )))
                            .is_err()
                        {
                            error!("Failed error response for triggered commit for account '{}'", pubkey);
                        } else {
                            debug!("Completed error response for trigger to commit '{}' ", pubkey);
                        }
                        continue;
                    }
                };
                debug_assert!(
                    commit_infos.len() <= 1,
                    "Manual trigger creates one transaction only"
                );
                match signatures.into_iter().next() {
                    Some((pubkey, signature)) => {
                        // Let the trigger transaction finish even though we didn't run the commit
                        // transaction yet. The signature will allow the client to verify the outcome.
                        communicate_trigger_success(
                            tx,
                            TriggerCommitOutcome::Committed(signature),
                            &pubkey,
                            Some(&signature),
                        );
                    }
                    None => {
                        // If the account state did not change then no commmit is necessary
                        communicate_trigger_success(
                            tx,
                            TriggerCommitOutcome::NotCommitted,
                            &pubkey,
                            None,
                        );
                        continue;
                    }
                };

                // Now after we informed the commit trigger transaction that all went well
                // so far we send and confirm the actual transaction to commit the account state.
                if let Err(ref err) = manager
                    .run_transactions_to_commit_specific_accounts(
                        now,
                        commit_infos,
                    )
                    .await
                {
                    use AccountsError::*;
                    let context = match err {
                        InvalidRpcUrl(msg)
                        | FailedToGetLatestBlockhash(msg)
                        | FailedToSendTransaction(msg) => {
                            format!("{} ({:?})", msg, err)
                        }
                        _ => format!("{:?}", err),
                    };
                    // The trigger transaction already finished, so we cannot inform
                    // it of the failure. The trigger issuer can find the transaction via its
                    // signature and will see the issue.
                    error!(
                        "Failed to commit account '{}' due to '{}'",
                        pubkey, context
                    );
                }
            }
        });
    }
}
