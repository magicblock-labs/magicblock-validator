use std::{collections::HashSet, path::Path, sync::Arc};

use dlp_api::pda::ephemeral_balance_pda_from_payer;
use errors::{ChainlinkError, ChainlinkResult};
use fetch_cloner::FetchCloner;
use magicblock_accounts_db::{
    traits::AccountsBank, AccountsDb, AccountsDbResult,
};
use magicblock_aml::RiskService;
use magicblock_config::config::ChainLinkConfig;
use magicblock_metrics::metrics::AccountFetchOrigin;
use solana_account::{AccountSharedData, ReadableAccount};
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_sdk_ids::feature;
use solana_signer::Signer;
use solana_transaction::sanitized::SanitizedTransaction;
use tokio::{sync::mpsc, task};
use tracing::*;

use crate::{
    cloner::Cloner,
    config::ChainlinkConfig,
    fetch_cloner::FetchAndCloneResult,
    filters::is_noop_system_transfer,
    remote_account_provider::{
        chain_pubsub_client::mock::ChainPubsubClientMock,
        chain_updates_client::ChainUpdatesClient, ChainPubsubClient,
        ChainRpcClient, ChainRpcClientImpl, Endpoints, RemoteAccountProvider,
    },
    submux::SubMuxClient,
    testing::{cloner_stub::ClonerStub, rpc_client_mock::ChainRpcClientMock},
};

mod account_still_undelegating_on_chain;
mod blacklisted_accounts;
pub mod config;
pub mod errors;
pub mod fetch_cloner;

pub use blacklisted_accounts::*;

/// A type alias for chainlink with only accountsdb being real impl
pub type StubbedChainlink<V> =
    Chainlink<ChainRpcClientMock, ChainPubsubClientMock, V, ClonerStub>;

/// Production Chainlink stack with configurable cloner implementation.
pub type DefaultRealChainlink<C> = Chainlink<
    ChainRpcClientImpl,
    SubMuxClient<ChainUpdatesClient>,
    AccountsDb,
    C,
>;

/// Production mode-aware Chainlink stack with configurable cloner implementation.
pub type DefaultModeAwareChainlink<C> = ModeAwareChainlink<
    ChainRpcClientImpl,
    SubMuxClient<ChainUpdatesClient>,
    AccountsDb,
    C,
>;

/// Narrow reset-only bridge for services that need account-bank cleanup
/// without depending on a Chainlink-shaped test stub.
pub trait AccountsBankResetter: Send + Sync {
    fn reset_accounts_bank(&self) -> AccountsDbResult<()>;
}

// -----------------
// Chainlink
// -----------------
pub struct Chainlink<
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
> {
    accounts_bank: Arc<V>,
    fetch_cloner: Option<Arc<FetchCloner<T, U, V, C>>>,
    /// The subscription to events for each account that is removed from
    /// the accounts tracked by the provider.
    /// In that case we also remove it from the bank since it is no longer
    /// synchronized.
    #[allow(unused)] // needed to cleanup chainlink
    removed_accounts_sub: Option<task::JoinHandle<()>>,

    validator_id: Pubkey,

    /// If true, remove confined accounts during bank reset
    remove_confined_accounts: bool,
}

pub enum ModeAwareChainlink<
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
> {
    Enabled(Chainlink<T, U, V, C>),
    Disabled(StubbedChainlink<V>),
}

impl<T: ChainRpcClient, U: ChainPubsubClient, V: AccountsBank, C: Cloner>
    ModeAwareChainlink<T, U, V, C>
{
    pub fn enabled(chainlink: Chainlink<T, U, V, C>) -> Self {
        Self::Enabled(chainlink)
    }

    pub fn disabled(
        accounts_bank: &Arc<V>,
        validator_pubkey: Pubkey,
        config: &ChainLinkConfig,
    ) -> ChainlinkResult<Self> {
        let chainlink = StubbedChainlink::try_new(
            accounts_bank,
            None,
            validator_pubkey,
            config,
        )?;
        Ok(Self::Disabled(chainlink))
    }

    pub fn reset_accounts_bank(&self) -> AccountsDbResult<()> {
        match self {
            Self::Enabled(chainlink) => chainlink.reset_accounts_bank(),
            Self::Disabled(chainlink) => chainlink.reset_accounts_bank(),
        }
    }

    pub async fn ensure_accounts(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        fetch_origin: AccountFetchOrigin,
        program_ids: Option<&[Pubkey]>,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        match self {
            Self::Enabled(chainlink) => {
                chainlink
                    .ensure_accounts(
                        pubkeys,
                        mark_empty_if_not_found,
                        fetch_origin,
                        program_ids,
                    )
                    .await
            }
            Self::Disabled(chainlink) => {
                chainlink
                    .ensure_accounts(
                        pubkeys,
                        mark_empty_if_not_found,
                        fetch_origin,
                        program_ids,
                    )
                    .await
            }
        }
    }

    pub async fn ensure_transaction_accounts(
        &self,
        tx: &SanitizedTransaction,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        match self {
            Self::Enabled(chainlink) => {
                chainlink.ensure_transaction_accounts(tx).await
            }
            Self::Disabled(_) => Err(ChainlinkError::DisabledForNonPrimaryMode),
        }
    }

    pub async fn fetch_accounts(
        &self,
        pubkeys: &[Pubkey],
        fetch_origin: AccountFetchOrigin,
        program_ids: Option<&[Pubkey]>,
    ) -> ChainlinkResult<Vec<Option<AccountSharedData>>> {
        match self {
            Self::Enabled(chainlink) => {
                chainlink
                    .fetch_accounts(pubkeys, fetch_origin, program_ids)
                    .await
            }
            Self::Disabled(chainlink) => {
                chainlink
                    .fetch_accounts(pubkeys, fetch_origin, program_ids)
                    .await
            }
        }
    }

    pub async fn undelegation_requested(
        &self,
        pubkey: Pubkey,
    ) -> ChainlinkResult<()> {
        match self {
            Self::Enabled(chainlink) => {
                chainlink.undelegation_requested(pubkey).await
            }
            Self::Disabled(_) => Ok(()),
        }
    }

    pub fn fetch_count(&self) -> Option<u64> {
        match self {
            Self::Enabled(chainlink) => chainlink.fetch_count(),
            Self::Disabled(chainlink) => chainlink.fetch_count(),
        }
    }

    pub fn is_watching(&self, pubkey: &Pubkey) -> bool {
        match self {
            Self::Enabled(chainlink) => chainlink.is_watching(pubkey),
            Self::Disabled(chainlink) => chainlink.is_watching(pubkey),
        }
    }

    /// This exists only for the replication service to get a Chainlink-shaped
    /// handle for reset_accounts_bank(). It must not be used for RPC fetching,
    /// subscriptions, or remote Chainlink work. This is temporary and should be
    /// removed once account-bank cleanup is moved out of Chainlink and into
    /// AccountsDb/account-cleanup code.
    pub fn stub(&self) -> StubbedChainlink<V> {
        match self {
            Self::Enabled(chainlink) => chainlink.stub(),
            Self::Disabled(chainlink) => chainlink.stub(),
        }
    }
}

impl<T: ChainRpcClient, U: ChainPubsubClient, V: AccountsBank, C: Cloner>
    AccountsBankResetter for ModeAwareChainlink<T, U, V, C>
{
    fn reset_accounts_bank(&self) -> AccountsDbResult<()> {
        ModeAwareChainlink::reset_accounts_bank(self)
    }
}

impl<T: ChainRpcClient, U: ChainPubsubClient, V: AccountsBank, C: Cloner>
    Chainlink<T, U, V, C>
{
    pub fn try_new(
        accounts_bank: &Arc<V>,
        fetch_cloner: Option<Arc<FetchCloner<T, U, V, C>>>,
        validator_pubkey: Pubkey,
        config: &ChainLinkConfig,
    ) -> ChainlinkResult<Self> {
        let removed_accounts_sub = if let Some(fetch_cloner) = &fetch_cloner {
            let removed_accounts_rx =
                fetch_cloner.try_get_removed_account_rx()?;
            let cloner = fetch_cloner.cloner();
            Some(Self::subscribe_account_removals(
                accounts_bank,
                cloner,
                removed_accounts_rx,
            ))
        } else {
            None
        };
        Ok(Self {
            accounts_bank: accounts_bank.clone(),
            fetch_cloner,
            removed_accounts_sub,
            validator_id: validator_pubkey,
            remove_confined_accounts: config.remove_confined_accounts,
        })
    }

    #[allow(clippy::too_many_arguments)]
    #[instrument(skip(
        endpoints,
        accounts_bank,
        cloner,
        config,
        chainlink_config
    ))]
    pub async fn try_new_from_endpoints(
        endpoints: &Endpoints,
        commitment: CommitmentConfig,
        accounts_bank: &Arc<V>,
        cloner: &Arc<C>,
        validator_keypair: Keypair,
        config: ChainlinkConfig,
        chainlink_config: &ChainLinkConfig,
        ledger_path: &Path,
    ) -> ChainlinkResult<
        Chainlink<ChainRpcClientImpl, SubMuxClient<ChainUpdatesClient>, V, C>,
    > {
        let validator_pubkey = validator_keypair.pubkey();
        // Extract accounts provider and create fetch cloner while connecting
        // the subscription channel
        let (tx, rx) = tokio::sync::mpsc::channel(5_000);
        let account_provider = RemoteAccountProvider::try_from_urls_and_config(
            endpoints,
            commitment,
            tx,
            &config.remote_account_provider,
        )
        .await?;
        let fetch_cloner = if let Some(provider) = account_provider {
            let provider = Arc::new(provider);
            let risk_service = RiskService::try_from_config(
                &chainlink_config.risk,
                ledger_path,
            )?
            .map(Arc::new);
            let fetch_cloner = FetchCloner::new(
                &provider,
                accounts_bank,
                cloner,
                validator_keypair,
                rx,
                chainlink_config.allowed_programs.clone(),
                risk_service,
            );
            Some(fetch_cloner)
        } else {
            None
        };

        Chainlink::try_new(
            accounts_bank,
            fetch_cloner,
            validator_pubkey,
            chainlink_config,
        )
    }

    /// Removes all accounts that aren't delegated to us and not blacklisted from the bank
    /// This should only be called _before_ the validator starts up, i.e.
    /// when resuming an existing ledger to guarantee that we don't hold
    /// accounts that might be stale.
    pub fn reset_accounts_bank(&self) -> AccountsDbResult<()> {
        let blacklisted_accounts = blacklisted_accounts(&self.validator_id);

        let mut delegated_only = 0;
        let mut kept_ephemeral = 0;
        let mut undelegating = 0;
        let mut blacklisted = 0;
        let mut remaining = 0u32;

        let removed = self.accounts_bank.remove_where(|pubkey, account| {
            if blacklisted_accounts.contains(pubkey) {
                blacklisted += 1;
                return false;
            }
            if self.remove_confined_accounts && account.confined() {
                return true;
            }
            // Undelegating accounts are normally also delegated, but if that ever changes
            // we want to make sure we never remove an account of which we aren't sure
            // if the undelegation completed on chain or not.
            let should_remove = if account.undelegating() {
                undelegating += 1;
                false
            } else if account.ephemeral() {
                kept_ephemeral += 1;
                false
            } else if account.delegated() {
                delegated_only += 1;
                false
            } else {
                *account.owner() != feature::ID
            };
            if should_remove {
                trace!(
                    pubkey = %pubkey,
                    account=%format!("{account:#?}"),
                    "Removing non-delegated account during accountsdb reset"
                );
            } else {
                remaining += 1;
            }
            should_remove
        })?;

        info!(
            total_removed = removed,
            delegated_not_undelegating = delegated_only,
            delegated_and_undelegating = undelegating,
            kept_delegated = delegated_only,
            kept_blacklisted = blacklisted,
            kept_ephemeral,
            "Removed accounts from bank"
        );
        Ok(())
    }

    fn subscribe_account_removals(
        accounts_bank: &Arc<V>,
        cloner: &Arc<C>,
        mut removed_accounts_rx: mpsc::Receiver<Pubkey>,
    ) -> task::JoinHandle<()> {
        let accounts_bank = accounts_bank.clone();
        let cloner = cloner.clone();

        task::spawn(async move {
            while let Some(pubkey) = removed_accounts_rx.recv().await {
                // Pre-flight check: skip if delegated/undelegating
                // (the processor enforces this too, but this avoids
                // the overhead of building and submitting a doomed tx)
                let should_evict = match accounts_bank.get_account(&pubkey) {
                    Some(account) => {
                        let undelegating = account.undelegating();
                        let delegated = account.delegated();
                        let evict = !undelegating && !delegated;
                        if !evict {
                            trace!(
                                pubkey = %pubkey,
                                undelegating,
                                delegated,
                                owner = %account.owner(),
                                "Keeping unsubscribed account \
                                 in bank \
                                 (delegated/undelegating)"
                            );
                        }
                        evict
                    }
                    None => false,
                };
                if !should_evict {
                    continue;
                }

                trace!(
                    pubkey = %pubkey,
                    "Submitting eviction transaction"
                );
                if let Err(err) = cloner.evict_account(pubkey).await {
                    warn!(
                        pubkey = %pubkey,
                        error = ?err,
                        "Failed to submit eviction transaction"
                    );
                }
            }
            warn!("Removed accounts channel closed");
        })
    }

    /// This method ensures that the accounts rise to the top of used accounts, no
    /// matter if we end up cloning/subscribing to them or not.
    /// For new accounts this would not be needed as they are promoted when
    /// they are added, but for existing accounts that step is never taken.
    /// For those accounts that weren't subscribed to yet (new accounts) this
    /// does nothing as only existing accounts are affected.
    /// See [lru::LruCache::promote]
    fn promote_accounts(
        fetch_cloner: &FetchCloner<T, U, V, C>,
        pubkeys: &[&Pubkey],
    ) {
        fetch_cloner.promote_accounts(pubkeys);
    }

    /// Ensures that all accounts required by the transaction exist on chain,
    /// are delegated to our validator if writable and that their latest state
    /// is cloned in our validator.
    /// Returns the state of each account (writable and readonly) after the checks
    /// and cloning are done.
    #[instrument(skip(self, tx))]
    pub async fn ensure_transaction_accounts(
        &self,
        tx: &SanitizedTransaction,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        if is_noop_system_transfer(tx) {
            trace!(
                tx_sig = %tx.signature(),
                "Skipping account ensure for noop system transfer transaction"
            );
            return Ok(Default::default());
        }

        let mut pubkeys = tx
            .message()
            .account_keys()
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let feepayer = tx.message().fee_payer();

        let balance_pda = ephemeral_balance_pda_from_payer(feepayer, 0);

        // Determine if we need to clone the escrow account for the feepayer
        let clone_escrow =
            self.accounts_bank.get_account(&balance_pda).is_none();

        // If cloning escrow, add the balance PDA
        if clone_escrow {
            trace!(
                balance_pda = %balance_pda,
                feepayer = %feepayer,
                "Adding balance PDA for feepayer"
            );
            pubkeys.push(balance_pda);
        }

        // Mark *all* pubkeys as empty-if-not-found
        let mark_empty_if_not_found = Some(pubkeys.as_slice());

        // Extract programs from transaction instructions for metrics
        let program_ids = extract_program_ids_from_transaction(tx);

        // Ensure accounts
        let res = self
            .ensure_accounts(
                &pubkeys,
                mark_empty_if_not_found,
                AccountFetchOrigin::SendTransaction(*tx.signature()),
                Some(&program_ids),
            )
            .await?;

        Ok(res)
    }

    /// Same as fetch accounts, but does not return the accounts, just
    /// ensures were cloned into our validator if they exist on chain.
    /// If we're offline and not syncing accounts then this is a no-op.
    #[instrument(skip(self, pubkeys, mark_empty_if_not_found, program_ids))]
    pub async fn ensure_accounts(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        fetch_origin: AccountFetchOrigin,
        program_ids: Option<&[Pubkey]>,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        let Some(fetch_cloner) = self.fetch_cloner() else {
            return Ok(FetchAndCloneResult::default());
        };
        self.fetch_accounts_common(
            fetch_cloner,
            pubkeys,
            mark_empty_if_not_found,
            fetch_origin,
            program_ids,
        )
        .await
    }

    /// Fetches the accounts from the bank if we're offline and not syncing accounts.
    /// Otherwise ensures that the accounts exist on chain and were cloned into our validator
    /// and returns their state from the bank (which may be None if the account does not
    /// exist locally or on chain).
    #[instrument(skip(self, pubkeys, program_ids))]
    pub async fn fetch_accounts(
        &self,
        pubkeys: &[Pubkey],
        fetch_origin: AccountFetchOrigin,
        program_ids: Option<&[Pubkey]>,
    ) -> ChainlinkResult<Vec<Option<AccountSharedData>>> {
        if tracing::enabled!(tracing::Level::TRACE) {
            let count = pubkeys.len();
            trace!(count, "Fetching accounts");
        }
        let Some(fetch_cloner) = self.fetch_cloner() else {
            // If we're offline and not syncing accounts then we just get them from the bank
            return Ok(pubkeys
                .iter()
                .map(|pubkey| self.accounts_bank.get_account(pubkey))
                .collect());
        };
        let _ = self
            .fetch_accounts_common(
                fetch_cloner,
                pubkeys,
                None,
                fetch_origin,
                program_ids,
            )
            .await?;

        let accounts = pubkeys
            .iter()
            .map(|pubkey| self.accounts_bank.get_account(pubkey))
            .collect();
        Ok(accounts)
    }

    #[instrument(skip(
        self,
        fetch_cloner,
        pubkeys,
        mark_empty_if_not_found,
        program_ids
    ))]
    async fn fetch_accounts_common(
        &self,
        fetch_cloner: &FetchCloner<T, U, V, C>,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        fetch_origin: AccountFetchOrigin,
        program_ids: Option<&[Pubkey]>,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        if tracing::enabled!(tracing::Level::TRACE) {
            let count = pubkeys.len();
            let mark_empty_count = mark_empty_if_not_found.map(|k| k.len());
            trace!(count, mark_empty_count, "Fetching accounts");
        }
        Self::promote_accounts(
            fetch_cloner,
            &pubkeys.iter().collect::<Vec<_>>(),
        );

        // If any of the accounts was invalid and couldn't be fetched/cloned then
        // we return an error.
        let result = fetch_cloner
            .fetch_and_clone_accounts_with_dedup(
                pubkeys,
                mark_empty_if_not_found,
                None,
                fetch_origin,
                program_ids,
            )
            .await?;
        trace!("Fetched and cloned accounts");
        Ok(result)
    }

    /// This is called via the committor service when an account is about to be undelegated
    /// At this point we do the following:
    /// 1. Subscribe to updates for the account
    /// 2. When a subscription update is received we clone the new state as usual
    #[instrument(skip(self))]
    pub async fn undelegation_requested(
        &self,
        pubkey: Pubkey,
    ) -> ChainlinkResult<()> {
        debug!(pubkey = %pubkey, "Undelegation requested");

        magicblock_metrics::metrics::inc_undelegation_requested();

        let Some(fetch_cloner) = self.fetch_cloner() else {
            return Ok(());
        };

        // Subscribe to updates for this account so we can track changes
        // once it's undelegated
        fetch_cloner
            .subscribe_to_account_to_track_undelegation(&pubkey)
            .await?;

        debug!(pubkey = %pubkey, "Successfully subscribed for undelegation tracking");
        Ok(())
    }

    pub fn fetch_cloner(&self) -> Option<&Arc<FetchCloner<T, U, V, C>>> {
        self.fetch_cloner.as_ref()
    }

    pub fn fetch_count(&self) -> Option<u64> {
        self.fetch_cloner().map(|provider| provider.fetch_count())
    }

    pub fn is_watching(&self, pubkey: &Pubkey) -> bool {
        self.fetch_cloner()
            .map(|provider| provider.is_watching(pubkey))
            .unwrap_or(false)
    }

    /// A temporary hacky method to clone chainlink with accountsdb only,
    /// for it's used by the replication service to clean up accountsdb
    ///
    /// TODO(bmuddha):
    /// remove all accountsdb management from chainlink, after accountsdb refactoring
    pub fn stub(&self) -> StubbedChainlink<V> {
        Chainlink {
            accounts_bank: self.accounts_bank.clone(),
            fetch_cloner: None,
            removed_accounts_sub: None,
            validator_id: self.validator_id,
            remove_confined_accounts: self.remove_confined_accounts,
        }
    }
}

// -----------------
// Helper Functions
// -----------------

/// Extracts all unique program IDs from a transaction's instructions.
fn extract_program_ids_from_transaction(
    tx: &SanitizedTransaction,
) -> Vec<Pubkey> {
    let program_ids = tx
        .message()
        .program_instructions_iter()
        .map(|(program_id, _)| *program_id)
        .collect::<HashSet<_>>();
    program_ids.into_iter().collect()
}

#[cfg(test)]
mod mode_aware_tests {
    use std::sync::Arc;

    use magicblock_accounts_db::traits::AccountsBank;
    use magicblock_config::config::ChainLinkConfig;
    use magicblock_metrics::metrics::AccountFetchOrigin;
    use solana_message::legacy::Message;
    use solana_pubkey::Pubkey;
    use solana_transaction::{sanitized::SanitizedTransaction, Transaction};

    use super::{errors::ChainlinkError, ModeAwareChainlink};
    use crate::{
        accounts_bank::mock::AccountsBankStub,
        remote_account_provider::chain_pubsub_client::mock::ChainPubsubClientMock,
        testing::{
            cloner_stub::ClonerStub, rpc_client_mock::ChainRpcClientMock,
        },
    };

    type TestModeAwareChainlink = ModeAwareChainlink<
        ChainRpcClientMock,
        ChainPubsubClientMock,
        AccountsBankStub,
        ClonerStub,
    >;

    fn disabled_chainlink() -> (Arc<AccountsBankStub>, TestModeAwareChainlink) {
        let accounts_bank = Arc::new(AccountsBankStub::default());
        let chainlink = TestModeAwareChainlink::disabled(
            &accounts_bank,
            Pubkey::new_unique(),
            &ChainLinkConfig::default(),
        )
        .expect("disabled Chainlink should be constructed");
        (accounts_bank, chainlink)
    }

    #[tokio::test]
    async fn disabled_mode_ensure_accounts_is_noop() {
        let (accounts_bank, chainlink) = disabled_chainlink();
        let pubkey = Pubkey::new_unique();

        let result = chainlink
            .ensure_accounts(
                &[pubkey],
                None,
                AccountFetchOrigin::GetAccount,
                None,
            )
            .await;

        assert!(result
            .expect("disabled ensure_accounts should succeed")
            .is_ok());
        assert!(accounts_bank.get_account(&pubkey).is_none());
        assert_eq!(chainlink.fetch_count(), None);
        assert!(!chainlink.is_watching(&pubkey));
    }

    #[tokio::test]
    async fn disabled_mode_rejects_transaction_ensure() {
        let (_accounts_bank, chainlink) = disabled_chainlink();
        let payer = Pubkey::new_unique();
        let message = Message::new(&[], Some(&payer));
        let tx = Transaction::new_unsigned(message);
        let sanitized_tx = SanitizedTransaction::from_transaction_for_tests(tx);

        let error = chainlink
            .ensure_transaction_accounts(&sanitized_tx)
            .await
            .expect_err("disabled transaction ensure should be rejected");

        assert!(matches!(error, ChainlinkError::DisabledForNonPrimaryMode));
    }
}
