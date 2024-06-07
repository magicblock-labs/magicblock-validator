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
    commit_sender::TriggerCommitReceiver,
    errors::{MagicError, MagicErrorWithContext},
};
use sleipnir_transaction_status::TransactionStatusSender;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    transaction::SanitizedTransaction,
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
        mut rcvr: TriggerCommitReceiver,
    ) {
        let manager = manager.clone();
        tokio::spawn(async move {
            while let Some((pubkey, tx)) = rcvr.recv().await {
                let now = get_epoch();
                match manager.commit_specific_accounts(now, vec![pubkey]).await
                {
                    Ok(sigs) => {
                        let sig = sigs.first().cloned().unwrap_or_default();
                        if tx.send(Ok(sig)).is_err() {
                            error!("Failed to acknowledge triggered commit for account '{}'", pubkey);
                        } else {
                            debug!("Completed trigger to commit '{}' with signature '{}'", pubkey, sig);
                        }
                    }
                    Err(ref err) => {
                        use AccountsError::*;
                        let context = match err {
                            InvalidRpcUrl(msg)
                            | FailedToGetLatestBlockhash(msg)
                            | FailedToSendAndConfirmTransaction(msg) => {
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
                    }
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
        if self.external_readonly_mode.clone_none()
            && self.external_writable_mode.clone_none()
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
        // 2. Remove all accounts we already track as external accounts
        //    and the ones that are found in our validator
        let new_readonly_accounts = if self.external_readonly_mode.clone_none()
        {
            vec![]
        } else {
            accounts_holder
                .readonly
                .into_iter()
                // 1. Filter external readonly accounts we already know about and cloned
                //    They would also be found via the internal account provider, but this
                //    is a faster lookup
                .filter(|pubkey| !self.external_readonly_accounts.has(pubkey))
                // 2. Filter accounts that are found inside our validator (slower looukup)
                .filter(|pubkey| {
                    self.internal_account_provider.get_account(pubkey).is_none()
                })
                .collect::<Vec<_>>()
        };
        trace!("New readonly accounts: {:?}", new_readonly_accounts);

        let new_writable_accounts = if self.external_writable_mode.clone_none()
        {
            vec![]
        } else {
            accounts_holder
                .writable
                .into_iter()
                .filter(|pubkey| !self.external_writable_accounts.has(pubkey))
                .filter(|pubkey| {
                    self.internal_account_provider.get_account(pubkey).is_none()
                })
                .collect::<Vec<_>>()
        };
        trace!("New writable accounts: {:?}", new_writable_accounts);

        // 3. Validate only the accounts that we see for the very first time
        let validated_accounts = self
            .validated_accounts_provider
            .validate_accounts(
                &TransactionAccountsHolder {
                    readonly: new_readonly_accounts,
                    writable: new_writable_accounts,
                    payer: accounts_holder.payer,
                },
                &ValidateAccountsConfig {
                    allow_new_accounts: self.create_accounts,
                    // Here we specify if we can clone all writable accounts or
                    // only the ones that were delegated
                    require_delegation: self
                        .external_writable_mode
                        .clone_delegated_only(),
                },
            )
            .await?;

        // 4. If a readonly account is not a program, but we only should clone programs then
        //    we have a problem since the account does not exist nor will it be created.
        //    Here we just remove it from the accounts to be cloned and let the  trigger
        //    transaction fail due to the missing account as it normally would.
        //    We have a similar problem if the account was not found at all in which case
        //    it's `is_program` field is `None`.
        let programs_only = self.external_readonly_mode.clone_programs_only();
        let readonly_clones = validated_accounts
            .readonly
            .iter()
            .flat_map(|acc| {
                if acc.is_program.is_none() {
                    None
                } else if !programs_only || acc.is_program == Some(true) {
                    Some(acc.pubkey)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        let writable_clones = validated_accounts
            .writable
            .iter()
            .filter(|x| !x.is_new)
            .collect::<Vec<_>>();

        // Useful logging of updated writable/readables
        if log::log_enabled!(log::Level::Debug) {
            if !readonly_clones.is_empty() {
                debug!(
                    "Transaction '{}' triggered readonly account clones: {:?}",
                    signature, readonly_clones,
                );
            }
            if !writable_clones.is_empty() {
                let writable = validated_accounts
                    .writable
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
                    signature, writable
                );
            }
        }

        // 5. Clone the accounts and add metadata to external account trackers
        let mut signatures = vec![];
        for readonly in readonly_clones {
            let signature =
                self.account_cloner.clone_account(&readonly, None).await?;
            signatures.push(signature);
            self.external_readonly_accounts.insert(readonly);
        }

        for writable in writable_clones {
            let mut overrides =
                writable.lock_config.as_ref().map(|x| AccountModification {
                    owner: Some(x.owner.to_string()),
                    ..Default::default()
                });
            if writable.is_payer {
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
                .clone_account(&writable.pubkey, overrides)
                .await?;
            signatures.push(signature);
            self.external_writable_accounts.insert(
                writable.pubkey,
                writable.lock_config.as_ref().map(|x| x.commit_frequency),
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
        // 1. Find all accounts that are due to be committed let accounts_to_be_committed = self
        let accounts_to_be_committed = self
            .external_writable_accounts
            .read_accounts()
            .values()
            .filter(|x| x.needs_commit(now))
            .map(|x| x.pubkey)
            .collect::<Vec<_>>();
        self.commit_specific_accounts(now, accounts_to_be_committed)
            .await
    }

    async fn commit_specific_accounts(
        &self,
        now: Duration,
        accounts_to_be_committed: Vec<Pubkey>,
    ) -> AccountsResult<Vec<Signature>> {
        // 2. Get current account states from internal account provider
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

        // 3. Commit the accounts and mark them as committed
        let mut signatures = Vec::new();
        for (pubkey, state) in account_states {
            let sig =
                self.account_committer.commit_account(pubkey, state).await?;
            // If the last committed state is the same as the current state
            // then it wasn't committed.
            // In that case we don't mark it as such in order to trigger commitment
            // as soon as its state changes.
            if let Some(sig) = sig {
                signatures.push(sig);
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
            }
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
