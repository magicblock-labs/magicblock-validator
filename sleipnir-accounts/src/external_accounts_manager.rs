use std::{collections::HashMap, sync::Arc, time::Duration};

use conjunto_transwise::{
    transaction_accounts_holder::TransactionAccountsHolder,
    validated_accounts::ValidateAccountsConfig, RpcProviderConfig,
    TransactionAccountsExtractor, Transwise, ValidatedAccountsProvider,
};
use log::*;
use sleipnir_bank::bank::Bank;
use sleipnir_mutator::AccountModification;
use sleipnir_program::{
    commit_sender::{
        TriggerCommitCallback, TriggerCommitOutcome, TriggerCommitReceiver,
    },
    errors::{MagicError, MagicErrorWithContext},
};
use sleipnir_transaction_status::TransactionStatusSender;
use solana_rpc_client::{
    nonblocking::rpc_client::RpcClient, rpc_client::SerializableTransaction,
};
use solana_sdk::{
    account::AccountSharedData,
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    transaction::{SanitizedTransaction, Transaction},
};

use crate::{
    bank_account_provider::BankAccountProvider,
    config::{AccountsConfig, ExternalReadonlyMode, ExternalWritableMode},
    errors::{AccountsError, AccountsResult},
    external_accounts::{ExternalReadonlyAccounts, ExternalWritableAccounts},
    remote_account_cloner::RemoteAccountCloner,
    remote_account_committer::RemoteAccountCommitter,
    traits::{AccountCloner, AccountCommitter, InternalAccountProvider},
    utils::{get_epoch, try_rpc_cluster_from_cluster},
};

pub enum CommitAccountInfo {
    /// The account state was committed via the wrapped transaction
    Committed(CommitAccountTransaction),
    /// The account state was not committed since it did not change since the last commit
    NotCommitted,
}
impl CommitAccountInfo {
    pub fn signature(&self) -> Option<Signature> {
        use CommitAccountInfo::*;
        match self {
            Committed(info) => Some(*info.transaction.get_signature()),
            NotCommitted => None,
        }
    }
}

pub struct CommitAccountTransaction {
    pub transaction: Transaction,
    pub commit_state_data: AccountSharedData,
}

pub type AccountsManager = ExternalAccountsManager<
    BankAccountProvider,
    RemoteAccountCloner,
    RemoteAccountCommitter,
    Transwise,
>;

#[derive(Debug)]
pub struct ExternalAccountsManager<IAP, ACL, ACM, VAP>
where
    IAP: InternalAccountProvider,
    ACL: AccountCloner,
    ACM: AccountCommitter,
    VAP: ValidatedAccountsProvider + TransactionAccountsExtractor,
{
    pub internal_account_provider: IAP,
    pub account_cloner: ACL,
    pub account_committer: ACM,
    pub validated_accounts_provider: VAP,
    pub external_readonly_accounts: ExternalReadonlyAccounts,
    pub external_writable_accounts: ExternalWritableAccounts,
    pub external_readonly_mode: ExternalReadonlyMode,
    pub external_writable_mode: ExternalWritableMode,
    pub create_accounts: bool,
    pub payer_init_lamports: Option<u64>,
}

impl
    ExternalAccountsManager<
        BankAccountProvider,
        RemoteAccountCloner,
        RemoteAccountCommitter,
        Transwise,
    >
{
    pub fn try_new(
        bank: &Arc<Bank>,
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
            validated_accounts_provider,
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

impl<IAP, ACL, ACM, VAP> ExternalAccountsManager<IAP, ACL, ACM, VAP>
where
    IAP: InternalAccountProvider,
    ACL: AccountCloner,
    ACM: AccountCommitter,
    VAP: ValidatedAccountsProvider + TransactionAccountsExtractor,
{
    pub async fn ensure_accounts(
        &self,
        tx: &SanitizedTransaction,
    ) -> AccountsResult<Vec<Signature>> {
        // If this validator does not clone any accounts then we're done
        if self.external_readonly_mode.is_clone_none()
            && self.external_writable_mode.is_clone_none()
        {
            return Ok(vec![]);
        }

        // 1. Extract all acounts from the transaction
        let accounts_holder = self
            .validated_accounts_provider
            .try_accounts_from_sanitized_transaction(tx)?;

        self.ensure_accounts_from_holder(
            accounts_holder,
            tx.signature().to_string(),
        )
        .await
    }

    // Direct use for tests only
    pub async fn ensure_accounts_from_holder(
        &self,
        accounts_holder: TransactionAccountsHolder,
        signature: String,
    ) -> AccountsResult<Vec<Signature>> {
        // 2.A Collect all readonly accounts we've never seen before and need to clone as readonly
        let unseen_readonly_accounts = if self
            .external_readonly_mode
            .is_clone_none()
        {
            vec![]
        } else {
            accounts_holder
                .readonly
                .into_iter()
                // If an account has already been cloned to be used as readonly, no need to re-do it
                .filter(|pubkey| !self.external_readonly_accounts.has(pubkey))
                // If an account has already been cloned and prepared to be used as writable, it can also be used as readonly
                .filter(|pubkey| !self.external_writable_accounts.has(pubkey))
                // If somehow the account is already in the validator data for other reason, no need to re-download it
                .filter(|pubkey| {
                    // Slowest lookup filter is done last
                    !self.internal_account_provider.has_account(pubkey)
                })
                .collect::<Vec<_>>()
        };
        trace!(
            "Newly seen readonly accounts: {:?}",
            unseen_readonly_accounts
        );

        // 2.B Collect all writable accounts we've never seen before and need to clone and prepare as writable
        let unseen_writable_accounts = if self
            .external_writable_mode
            .is_clone_none()
        {
            vec![]
        } else {
            accounts_holder
                .writable
                .into_iter()
                // If an account has already been cloned and prepared to be used as writable, no need to re-do it
                .filter(|pubkey| !self.external_writable_accounts.has(pubkey))
                // Even if the account is already present in the validator,
                // we still need to prepare it so it can be used as a writable.
                // Because it may only be able to be used as a readonly until modified.
                .collect::<Vec<_>>()
        };
        trace!(
            "Newly seen writable accounts: {:?}",
            unseen_writable_accounts
        );

        // 3. Validate only the accounts that we see for the very first time
        let validated_accounts = self
            .validated_accounts_provider
            .validate_accounts(
                &TransactionAccountsHolder {
                    readonly: unseen_readonly_accounts,
                    writable: unseen_writable_accounts,
                    payer: accounts_holder.payer,
                },
                &ValidateAccountsConfig {
                    allow_new_accounts: self.create_accounts,
                    // Here we specify if we can clone all writable accounts or
                    // only the ones that were delegated
                    require_delegation: self
                        .external_writable_mode
                        .is_clone_delegated_only(),
                },
            )
            .await?;

        // 4.A If a readonly account is not a program, but we only should clone programs then
        //     we have a problem since the account does not exist nor will it be created.
        //     Here we just remove it from the accounts to be cloned and let the  trigger
        //     transaction fail due to the missing account as it normally would.
        //     We have a similar problem if the account was not found at all in which case
        //     it's `is_program` field is `None`.
        let programs_only =
            self.external_readonly_mode.is_clone_programs_only();

        let cloned_readonly_accounts = validated_accounts
            .readonly
            .into_iter()
            .filter(|acc| match &acc.account {
                // If it exists: Allow the account if its a program or if we allow non-programs to be cloned
                Some(account) => account.executable || !programs_only,
                // Otherwise, don't clone it
                _ => false,
            })
            .collect::<Vec<_>>();

        // 4.B We will want to make sure that all accounts that exist on chain and are writable have been cloned
        let cloned_writable_accounts = validated_accounts
            .writable
            .into_iter()
            .filter(|acc| acc.account.is_some())
            .collect::<Vec<_>>();

        // Useful logging of involved writable/readables pubkeys
        if log::log_enabled!(log::Level::Debug) {
            if !cloned_readonly_accounts.is_empty() {
                debug!(
                    "Transaction '{}' triggered readonly account clones: {:?}",
                    signature,
                    cloned_readonly_accounts
                        .iter()
                        .map(|acc| acc.pubkey)
                        .collect::<Vec<_>>(),
                );
            }
            if !cloned_writable_accounts.is_empty() {
                let cloned_writable_descriptions = cloned_writable_accounts
                    .iter()
                    .map(|x| {
                        format!(
                            "{}{}{}",
                            if x.is_payer { "[payer]:" } else { "" },
                            x.pubkey,
                            x.lock_config
                                .as_ref()
                                .map(|x| format!(
                                    ", owner: {}, commit_frequency: {}",
                                    x.owner, x.commit_frequency
                                ))
                                .unwrap_or("".to_string()),
                        )
                    })
                    .collect::<Vec<_>>();
                debug!(
                    "Transaction '{}' triggered writable account clones: {:?}",
                    signature, cloned_writable_descriptions
                );
            }
        }

        let mut signatures = vec![];

        // 5.A Clone the unseen readonly accounts without any modifications
        for cloned_readonly_account in cloned_readonly_accounts {
            let signature = self
                .account_cloner
                .clone_account(
                    &cloned_readonly_account.pubkey,
                    cloned_readonly_account.account,
                    None,
                )
                .await?;
            signatures.push(signature);
            self.external_readonly_accounts
                .insert(cloned_readonly_account.pubkey);
        }

        // 5.B Clone the unseen writable accounts and apply modifications so they represent
        //     the undelegated state they would have on chain, i.e. with the original owner
        for cloned_writable_account in cloned_writable_accounts {
            // Create and the transaction to dump data array, lamports and owner change to the local state
            let mut overrides = cloned_writable_account
                .lock_config
                .as_ref()
                .map(|x| AccountModification {
                    owner: Some(x.owner.to_string()),
                    ..Default::default()
                });
            if cloned_writable_account.is_payer {
                if let Some(lamports) = self.payer_init_lamports {
                    match overrides {
                        Some(ref mut x) => x.lamports = Some(lamports),
                        None => {
                            overrides = Some(AccountModification {
                                lamports: Some(lamports),
                                ..Default::default()
                            })
                        }
                    }
                }
            }
            let signature = self
                .account_cloner
                .clone_account(
                    &cloned_writable_account.pubkey,
                    cloned_writable_account.account,
                    overrides,
                )
                .await?;
            signatures.push(signature);
            // Remove the account from the readonlys and add it to writables
            self.external_readonly_accounts
                .remove(&cloned_writable_account.pubkey);
            self.external_writable_accounts.insert(
                cloned_writable_account.pubkey,
                cloned_writable_account
                    .lock_config
                    .as_ref()
                    .map(|x| x.commit_frequency),
            );
        }

        if log::log_enabled!(log::Level::Debug) && !signatures.is_empty() {
            debug!("Transactions {:?}", signatures,);
        }

        Ok(signatures)
    }

    /// This will look at the time that passed since the last commit and determine
    /// which accounts are due to be committed, perform that step for them
    /// and return the signatures of the transactions that were sent to the cluster.
    pub async fn commit_delegated(&self) -> AccountsResult<Vec<Signature>> {
        let now = get_epoch();
        // Find all accounts that are due to be committed let accounts_to_be_committed = self
        let accounts_to_be_committed = self
            .external_writable_accounts
            .read_accounts()
            .values()
            .filter(|x| x.needs_commit(now))
            .map(|x| x.pubkey)
            .collect::<Vec<_>>();
        let commit_infos = self
            .create_transactions_to_commit_specific_accounts(
                accounts_to_be_committed,
            )
            .await?;
        self.run_transactions_to_commit_specific_accounts(now, commit_infos)
            .await
    }

    async fn create_transactions_to_commit_specific_accounts(
        &self,
        accounts_to_be_committed: Vec<Pubkey>,
    ) -> AccountsResult<HashMap<Pubkey, CommitAccountInfo>> {
        // Get current account states from internal account provider
        let mut account_states = HashMap::new();
        for pubkey in &accounts_to_be_committed {
            let account_state =
                self.internal_account_provider.get_account(pubkey);
            if let Some(acc) = account_state {
                account_states.insert(*pubkey, acc);
            } else {
                error!(
                    "Cannot find state for account that needs to be committed '{}' ",
                    pubkey
                );
            }
        }
        // Get the transactions to commit each account
        let mut commit_infos = HashMap::new();
        for (pubkey, state) in account_states {
            let tx = self
                .account_committer
                .create_commit_account_transaction(pubkey, state.clone())
                .await?;
            let res = match tx {
                Some(tx) => {
                    CommitAccountInfo::Committed(CommitAccountTransaction {
                        transaction: tx,
                        commit_state_data: state,
                    })
                }
                None => CommitAccountInfo::NotCommitted,
            };
            commit_infos.insert(pubkey, res);
        }

        Ok(commit_infos)
    }

    async fn run_transactions_to_commit_specific_accounts(
        &self,
        now: Duration,
        commmit_infos: HashMap<Pubkey, CommitAccountInfo>,
    ) -> AccountsResult<Vec<Signature>> {
        let mut signatures = Vec::with_capacity(commmit_infos.len());
        // Commit the accounts and mark them as committed
        for (pubkey, info) in commmit_infos {
            use CommitAccountInfo::*;
            let CommitAccountTransaction {
                transaction,
                commit_state_data,
            } = match info {
                Committed(info) => info,
                // If the last committed state is the same as the current state
                // then it isn't committed.
                // In that case we also don't mark it as such in order to trigger
                // commitment as soon as its state changes.
                NotCommitted => continue,
            };
            let signature = self
                .account_committer
                .commit_account(pubkey, commit_state_data, transaction)
                .await?;
            if let Some(acc) =
                self.external_writable_accounts.read_accounts().get(&pubkey)
            {
                acc.mark_as_committed(now);
            } else {
                // This should never happen
                error!(
                    "Account '{}' disappeared while being committed",
                    pubkey
                );
            }
            signatures.push(signature);
        }

        Ok(signatures)
    }

    pub fn last_commit(&self, pubkey: &Pubkey) -> Option<Duration> {
        self.external_writable_accounts
            .read_accounts()
            .get(pubkey)
            .map(|x| x.last_committed_at())
    }
}
