use std::{
    path::Path,
    sync::{atomic::AtomicU64, Arc},
};

use dlp_api::pda::ephemeral_balance_pda_from_payer;
use errors::{ChainlinkError, ChainlinkResult};
use fetch_cloner::FetchCloner;
use magicblock_accounts_db::{traits::AccountsBank, AccountsDb};
use magicblock_aml::RiskService;
use magicblock_config::config::ChainLinkConfig;
use magicblock_metrics::metrics::AccountFetchOrigin;
use solana_account::{AccountSharedData, ReadableAccount};
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_transaction::sanitized::SanitizedTransaction;
use tokio::{sync::mpsc, task};
use tracing::*;

use crate::{
    cloner::Cloner,
    config::ChainlinkConfig,
    fetch_cloner::FetchAndCloneResult,
    filters::is_noop_system_transfer,
    remote_account_provider::{
        chain_updates_client::ChainUpdatesClient, ChainPubsubClient,
        ChainRpcClient, ChainRpcClientImpl, Endpoints, RemoteAccountProvider,
    },
    submux::SubMuxClient,
};

mod account_still_undelegating_on_chain;
mod blacklisted_accounts;
pub mod config;
pub mod errors;
pub mod fetch_cloner;

pub use blacklisted_accounts::*;

/// Production Chainlink stack with configurable cloner implementation.
pub type ProdInnerChainlink<C> = InnerChainlink<
    ChainRpcClientImpl,
    SubMuxClient<ChainUpdatesClient>,
    AccountsDb,
    C,
>;

/// Production `ProdChainlink` stack with configurable cloner implementation.
pub type ProdChainlink<C> = ReplicationModeAwareChainlink<
    ChainRpcClientImpl,
    SubMuxClient<ChainUpdatesClient>,
    AccountsDb,
    C,
>;

// -----------------
// Chainlink
// -----------------
pub struct InnerChainlink<
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
}

pub enum ReplicationModeAwareChainlink<
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
> {
    Enabled(InnerChainlink<T, U, V, C>),
    Disabled,
}

impl<T: ChainRpcClient, U: ChainPubsubClient, V: AccountsBank, C: Cloner>
    ReplicationModeAwareChainlink<T, U, V, C>
{
    pub fn enabled(chainlink: InnerChainlink<T, U, V, C>) -> Self {
        Self::Enabled(chainlink)
    }

    pub fn disabled() -> ChainlinkResult<Self> {
        Ok(Self::Disabled)
    }

    pub async fn ensure_accounts(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        fetch_origin: AccountFetchOrigin,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        match self {
            Self::Enabled(chainlink) => {
                chainlink
                    .ensure_accounts(
                        pubkeys,
                        mark_empty_if_not_found,
                        fetch_origin,
                    )
                    .await
            }
            Self::Disabled => Ok(Default::default()),
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
            Self::Disabled => Err(ChainlinkError::DisabledForNonPrimaryMode),
        }
    }

    pub async fn fetch_accounts(
        &self,
        pubkeys: &[Pubkey],
        fetch_origin: AccountFetchOrigin,
    ) -> ChainlinkResult<Vec<Option<AccountSharedData>>> {
        match self {
            Self::Enabled(chainlink) => {
                chainlink.fetch_accounts(pubkeys, fetch_origin).await
            }
            Self::Disabled => Ok(vec![None; pubkeys.len()]),
        }
    }

    pub async fn accounts_delegated_on_base_and_er(
        &self,
        pubkeys: &[Pubkey],
        fetch_origin: AccountFetchOrigin,
    ) -> ChainlinkResult<Vec<bool>> {
        match self {
            Self::Enabled(chainlink) => {
                chainlink
                    .accounts_delegated_on_base_and_er(pubkeys, fetch_origin)
                    .await
            }
            Self::Disabled => Ok(vec![false; pubkeys.len()]),
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
            Self::Disabled => Ok(()),
        }
    }

    pub fn fetch_count(&self) -> Option<u64> {
        match self {
            Self::Enabled(chainlink) => chainlink.fetch_count(),
            Self::Disabled => None,
        }
    }

    pub fn fetch_cloner(&self) -> Option<&Arc<FetchCloner<T, U, V, C>>> {
        match self {
            Self::Enabled(chainlink) => chainlink.fetch_cloner(),
            Self::Disabled => None,
        }
    }

    pub fn is_watching(&self, pubkey: &Pubkey) -> bool {
        match self {
            Self::Enabled(chainlink) => chainlink.is_watching(pubkey),
            Self::Disabled => false,
        }
    }
}

pub fn should_schedule_bank_eviction(account: &AccountSharedData) -> bool {
    !account.ephemeral() && !account.delegated() && !account.undelegating()
}

impl<T: ChainRpcClient, U: ChainPubsubClient, V: AccountsBank, C: Cloner>
    InnerChainlink<T, U, V, C>
{
    pub fn try_new(
        accounts_bank: &Arc<V>,
        fetch_cloner: Option<Arc<FetchCloner<T, U, V, C>>>,
    ) -> ChainlinkResult<Self> {
        let removed_accounts_sub = if let Some(fetch_cloner) = &fetch_cloner {
            let removed_accounts_rx =
                fetch_cloner.try_get_removed_account_rx()?;
            let cloner = fetch_cloner.cloner();
            Some(Self::subscribe_account_removals(
                accounts_bank,
                cloner,
                fetch_cloner.remote_account_provider(),
                removed_accounts_rx,
            ))
        } else {
            None
        };
        Ok(Self {
            accounts_bank: accounts_bank.clone(),
            fetch_cloner,
            removed_accounts_sub,
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
        chain_slot: Arc<AtomicU64>,
    ) -> ChainlinkResult<
        InnerChainlink<
            ChainRpcClientImpl,
            SubMuxClient<ChainUpdatesClient>,
            V,
            C,
        >,
    > {
        // Extract accounts provider and create fetch cloner while connecting
        // the subscription channel
        let (tx, rx) = tokio::sync::mpsc::channel(5_000);
        let account_provider = RemoteAccountProvider::try_from_urls_and_config(
            endpoints,
            commitment,
            tx,
            &config.remote_account_provider,
            Some(chain_slot),
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

        InnerChainlink::try_new(accounts_bank, fetch_cloner)
    }

    fn subscribe_account_removals(
        accounts_bank: &Arc<V>,
        cloner: &Arc<C>,
        remote_account_provider: &Arc<RemoteAccountProvider<T, U>>,
        mut removed_accounts_rx: mpsc::Receiver<Pubkey>,
    ) -> task::JoinHandle<()> {
        let accounts_bank = accounts_bank.clone();
        let cloner = cloner.clone();
        let remote_account_provider = remote_account_provider.clone();

        task::spawn(async move {
            while let Some(pubkey) = removed_accounts_rx.recv().await {
                // Pre-flight check: skip if delegated/undelegating
                // (the processor enforces this too, but this avoids
                // the overhead of building and submitting a doomed tx)
                let should_evict = match accounts_bank.get_account(&pubkey) {
                    Some(account) => {
                        let evict = should_schedule_bank_eviction(&account);
                        if !evict {
                            trace!(
                                pubkey = %pubkey,
                                ephemeral = account.ephemeral(),
                                undelegating = account.undelegating(),
                                delegated = account.delegated(),
                                owner = %account.owner(),
                                "Ignoring removal notification because bank \
                                 state is protected; no EvictAccount \
                                 transaction will be submitted"
                            );
                        }
                        evict
                    }
                    None => false,
                };
                // Skipping protected bank state is not a removal event; it
                // must not be translated into a downstream bank eviction.
                if !should_evict {
                    continue;
                }

                // Removal notifications can race with a new acquire_subscription for the same
                // pubkey. The provider helper holds the same per-pubkey subscription lock used
                // by acquire/release while it re-checks is_watching and submits eviction. This
                // prevents an EvictAccount transaction from being submitted after a fresh
                // subscription has made the account watched again, without blocking unrelated
                // pubkeys on the defensive-eviction slow path.
                let cloner = cloner.clone();
                let evicted = remote_account_provider
                    .evict_unwatched_with_subscription_lock(&pubkey, || async move {
                        trace!(
                            pubkey = %pubkey,
                            "Submitting eviction transaction for unwatched account"
                        );
                        if let Err(err) = cloner.evict_account(pubkey).await {
                            warn!(
                                pubkey = %pubkey,
                                error = ?err,
                                "Failed to submit eviction transaction"
                            );
                        }
                    })
                    .await;

                if !evicted {
                    trace!(
                        pubkey = %pubkey,
                        "Skipping removal notification because account is watched again"
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

        // Ensure accounts
        let res = self
            .ensure_accounts(
                &pubkeys,
                mark_empty_if_not_found,
                AccountFetchOrigin::SendTransaction(*tx.signature()),
            )
            .await?;

        Ok(res)
    }

    /// Same as fetch accounts, but does not return the accounts, just
    /// ensures were cloned into our validator if they exist on chain.
    /// If we're offline and not syncing accounts then this is a no-op.
    #[instrument(skip(self, pubkeys, mark_empty_if_not_found))]
    pub async fn ensure_accounts(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        fetch_origin: AccountFetchOrigin,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        let Some(fetch_cloner) = self.fetch_cloner() else {
            return Ok(FetchAndCloneResult::default());
        };
        self.fetch_accounts_common(
            fetch_cloner,
            pubkeys,
            mark_empty_if_not_found,
            fetch_origin,
        )
        .await
    }

    /// Fetches the accounts from the bank if we're offline and not syncing accounts.
    /// Otherwise ensures that the accounts exist on chain and were cloned into our validator
    /// and returns their state from the bank (which may be None if the account does not
    /// exist locally or on chain).
    #[instrument(skip(self, pubkeys))]
    pub async fn fetch_accounts(
        &self,
        pubkeys: &[Pubkey],
        fetch_origin: AccountFetchOrigin,
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
            .fetch_accounts_common(fetch_cloner, pubkeys, None, fetch_origin)
            .await?;

        let accounts = pubkeys
            .iter()
            .map(|pubkey| self.accounts_bank.get_account(pubkey))
            .collect();
        Ok(accounts)
    }

    #[instrument(skip(self, pubkeys))]
    pub async fn accounts_delegated_on_base_and_er(
        &self,
        pubkeys: &[Pubkey],
        fetch_origin: AccountFetchOrigin,
    ) -> ChainlinkResult<Vec<bool>> {
        let Some(fetch_cloner) = self.fetch_cloner() else {
            return Ok(vec![false; pubkeys.len()]);
        };
        let remote_accounts = fetch_cloner
            .fetch_remote_accounts(pubkeys, fetch_origin)
            .await?;
        if remote_accounts.len() != pubkeys.len() {
            return Err(ChainlinkError::UnexpectedAccountCount(format!(
                "expected {} remote accounts, got {}",
                pubkeys.len(),
                remote_accounts.len()
            )));
        }

        Ok(pubkeys
            .iter()
            .zip(remote_accounts)
            .map(|(pubkey, remote_account)| {
                let delegated_on_base =
                    remote_account.is_owned_by_delegation_program();
                let delegated_on_er = self
                    .accounts_bank
                    .get_account(pubkey)
                    .is_some_and(|account| {
                        account.delegated()
                            || account.owner().eq(&dlp_api::id())
                    });
                delegated_on_base && delegated_on_er
            })
            .collect())
    }

    #[instrument(skip(self, fetch_cloner, pubkeys, mark_empty_if_not_found))]
    async fn fetch_accounts_common(
        &self,
        fetch_cloner: &FetchCloner<T, U, V, C>,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        fetch_origin: AccountFetchOrigin,
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
}

// -----------------
// Helper Functions
// -----------------

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use magicblock_accounts_db::traits::AccountsBank;
    use magicblock_metrics::metrics::AccountFetchOrigin;
    use solana_account::AccountSharedData;
    use solana_message::legacy::Message;
    use solana_pubkey::Pubkey;
    use solana_transaction::{sanitized::SanitizedTransaction, Transaction};
    use tokio::sync::mpsc;

    use super::{
        errors::ChainlinkError, should_schedule_bank_eviction, InnerChainlink,
        ReplicationModeAwareChainlink,
    };
    use crate::{
        accounts_bank::mock::AccountsBankStub,
        remote_account_provider::{
            chain_pubsub_client::mock::ChainPubsubClientMock,
            SubscriptionReason,
        },
        testing::{
            cloner_stub::ClonerStub, init_logger,
            rpc_client_mock::ChainRpcClientMock,
        },
    };

    type TestReplicationModeAwareChainlink = ReplicationModeAwareChainlink<
        ChainRpcClientMock,
        ChainPubsubClientMock,
        AccountsBankStub,
        ClonerStub,
    >;

    async fn test_remote_account_provider() -> Arc<
        crate::remote_account_provider::RemoteAccountProvider<
            ChainRpcClientMock,
            ChainPubsubClientMock,
        >,
    > {
        use std::sync::atomic::AtomicU64;

        use crate::{
            remote_account_provider::{
                chain_slot::ChainSlot, RemoteAccountProvider,
            },
            testing::{
                rpc_client_mock::ChainRpcClientMockBuilder,
                utils::create_test_lru_cache,
            },
        };

        let rpc_client = ChainRpcClientMockBuilder::new()
            .slot(1)
            .clock_sysvar_for_slot(1)
            .build();
        let (updates_sender, updates_receiver) = mpsc::channel(1_000);
        let pubsub_client =
            ChainPubsubClientMock::new(updates_sender, updates_receiver);
        let (forward_tx, _forward_rx) = mpsc::channel(1_000);
        let (subscribed_accounts, config) = create_test_lru_cache(1000);
        let chain_slot = Arc::<AtomicU64>::default();

        Arc::new(
            RemoteAccountProvider::new(
                rpc_client,
                pubsub_client,
                forward_tx,
                &config,
                subscribed_accounts,
                ChainSlot::new(chain_slot),
            )
            .await
            .expect("test remote account provider should be constructed"),
        )
    }

    fn disabled_chainlink(
    ) -> (Arc<AccountsBankStub>, TestReplicationModeAwareChainlink) {
        let accounts_bank = Arc::new(AccountsBankStub::default());
        let chainlink = TestReplicationModeAwareChainlink::disabled()
            .expect("disabled Chainlink should be constructed");
        (accounts_bank, chainlink)
    }

    #[tokio::test]
    async fn disabled_mode_ensure_accounts_is_noop() {
        let (accounts_bank, chainlink) = disabled_chainlink();
        let pubkey = Pubkey::new_unique();

        let result = chainlink
            .ensure_accounts(&[pubkey], None, AccountFetchOrigin::GetAccount)
            .await;

        assert!(result
            .expect("disabled ensure_accounts should succeed")
            .is_ok());
        assert!(accounts_bank.get_account(&pubkey).is_none());
        assert_eq!(chainlink.fetch_count(), None);
        assert!(!chainlink.is_watching(&pubkey));
    }

    #[tokio::test]
    async fn disabled_mode_fetch_accounts_returns_none_values() {
        let (_accounts_bank, chainlink) = disabled_chainlink();
        let pubkeys = vec![Pubkey::new_unique(), Pubkey::new_unique()];

        let result = chainlink
            .fetch_accounts(&pubkeys, AccountFetchOrigin::GetMultipleAccounts)
            .await;

        let accounts = result.expect("disabled fetch_accounts should succeed");
        assert_eq!(accounts.len(), 2);
        assert!(accounts.iter().all(Option::is_none));
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

    #[tokio::test]
    async fn test_subscribe_account_removals_skips_delegated_or_undelegating_and_evicts_normal(
    ) {
        init_logger();

        let accounts_bank = Arc::new(AccountsBankStub::default());
        let cloner = Arc::new(ClonerStub::new(accounts_bank.clone()));
        let (removed_tx, removed_rx) = mpsc::channel(8);
        let remote_account_provider = test_remote_account_provider().await;

        let delegated_pubkey = Pubkey::new_unique();
        let undelegating_pubkey = Pubkey::new_unique();
        let normal_pubkey = Pubkey::new_unique();
        let owner = Pubkey::new_unique();

        let mut delegated_account =
            AccountSharedData::new(1_000_000, 0, &owner);
        delegated_account.set_delegated(true);
        accounts_bank.insert(delegated_pubkey, delegated_account);

        let mut undelegating_account =
            AccountSharedData::new(1_000_000, 0, &owner);
        undelegating_account.set_undelegating(true);
        accounts_bank.insert(undelegating_pubkey, undelegating_account);

        let normal_account = AccountSharedData::new(1_000_000, 0, &owner);
        accounts_bank.insert(normal_pubkey, normal_account);

        let handle = InnerChainlink::<
            ChainRpcClientMock,
            ChainPubsubClientMock,
            AccountsBankStub,
            ClonerStub,
        >::subscribe_account_removals(
            &accounts_bank,
            &cloner,
            &remote_account_provider,
            removed_rx,
        );

        removed_tx.send(delegated_pubkey).await.unwrap();
        removed_tx.send(undelegating_pubkey).await.unwrap();
        removed_tx.send(normal_pubkey).await.unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if accounts_bank.get_account(&normal_pubkey).is_none() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("normal removal notification should submit eviction");

        assert!(accounts_bank.get_account(&delegated_pubkey).is_some());
        assert!(accounts_bank.get_account(&undelegating_pubkey).is_some());
        assert!(accounts_bank.get_account(&normal_pubkey).is_none());

        drop(removed_tx);
        handle.await.unwrap();
    }

    #[test]
    fn test_should_schedule_bank_eviction() {
        let owner = Pubkey::new_unique();
        let mut account = AccountSharedData::new(1, 0, &owner);
        assert!(should_schedule_bank_eviction(&account));

        account.set_delegated(true);
        assert!(!should_schedule_bank_eviction(&account));

        account.set_delegated(false);
        account.set_undelegating(true);
        assert!(!should_schedule_bank_eviction(&account));

        account.set_undelegating(false);
        account.set_ephemeral(true);
        assert!(!should_schedule_bank_eviction(&account));
    }

    #[tokio::test]
    async fn test_subscribe_account_removals_skips_ephemeral_and_evicts_normal()
    {
        init_logger();

        let accounts_bank = Arc::new(AccountsBankStub::default());
        let cloner = Arc::new(ClonerStub::new(accounts_bank.clone()));
        let (removed_tx, removed_rx) = mpsc::channel(8);
        let remote_account_provider = test_remote_account_provider().await;

        let ephemeral_pubkey = Pubkey::new_unique();
        let normal_pubkey = Pubkey::new_unique();
        let owner = Pubkey::new_unique();

        let mut ephemeral_account =
            AccountSharedData::new(1_000_000, 0, &owner);
        ephemeral_account.set_ephemeral(true);
        accounts_bank.insert(ephemeral_pubkey, ephemeral_account);

        let normal_account = AccountSharedData::new(1_000_000, 0, &owner);
        accounts_bank.insert(normal_pubkey, normal_account);

        let handle = InnerChainlink::<
            ChainRpcClientMock,
            ChainPubsubClientMock,
            AccountsBankStub,
            ClonerStub,
        >::subscribe_account_removals(
            &accounts_bank,
            &cloner,
            &remote_account_provider,
            removed_rx,
        );

        removed_tx.send(ephemeral_pubkey).await.unwrap();
        removed_tx.send(normal_pubkey).await.unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if accounts_bank.get_account(&normal_pubkey).is_none() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("normal removal notification should submit eviction");

        assert!(accounts_bank.get_account(&ephemeral_pubkey).is_some());
        assert!(accounts_bank.get_account(&normal_pubkey).is_none());

        drop(removed_tx);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_subscribe_account_removals_skips_evict_when_account_is_watched_again(
    ) {
        init_logger();

        let accounts_bank = Arc::new(AccountsBankStub::default());
        let cloner = Arc::new(ClonerStub::new(accounts_bank.clone()));
        let (removed_tx, removed_rx) = mpsc::channel(8);
        let remote_account_provider = test_remote_account_provider().await;

        let pubkey = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let account = AccountSharedData::new(1_000_000, 0, &owner);
        accounts_bank.insert(pubkey, account);

        let handle = InnerChainlink::<
            ChainRpcClientMock,
            ChainPubsubClientMock,
            AccountsBankStub,
            ClonerStub,
        >::subscribe_account_removals(
            &accounts_bank,
            &cloner,
            &remote_account_provider,
            removed_rx,
        );

        remote_account_provider
            .acquire_subscription(&pubkey, SubscriptionReason::DirectAccount)
            .await
            .expect("subscription acquisition should succeed");
        assert!(remote_account_provider.is_watching(&pubkey));

        removed_tx.send(pubkey).await.unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                tokio::time::sleep(Duration::from_millis(10)).await;
                assert!(accounts_bank.get_account(&pubkey).is_some());
                if remote_account_provider.is_watching(&pubkey) {
                    break;
                }
            }
        })
        .await
        .expect("watched account should not be evicted");

        assert!(remote_account_provider.is_watching(&pubkey));
        assert!(accounts_bank.get_account(&pubkey).is_some());

        drop(removed_tx);
        handle.await.unwrap();
        assert!(accounts_bank.get_account(&pubkey).is_some());
    }

    #[tokio::test]
    async fn test_defensive_eviction_blocks_same_pubkey_subscription_until_eviction_finishes(
    ) {
        init_logger();

        let remote_account_provider = test_remote_account_provider().await;
        let pubkey = Pubkey::new_unique();
        assert!(!remote_account_provider.is_watching(&pubkey));

        let eviction_started = Arc::new(tokio::sync::Notify::new());
        let release_eviction = Arc::new(tokio::sync::Notify::new());

        let eviction_provider = remote_account_provider.clone();
        let eviction_pubkey = pubkey;
        let eviction_started_for_task = eviction_started.clone();
        let release_eviction_for_task = release_eviction.clone();
        let eviction_task = tokio::spawn(async move {
            eviction_provider
                .evict_unwatched_with_subscription_lock(
                    &eviction_pubkey,
                    || async move {
                        eviction_started_for_task.notify_one();
                        release_eviction_for_task.notified().await;
                    },
                )
                .await
        });

        eviction_started.notified().await;

        let (result_tx, mut result_rx) = tokio::sync::oneshot::channel();
        let subscribe_provider = remote_account_provider.clone();
        let subscribe_pubkey = pubkey;
        let subscribe_task = tokio::spawn(async move {
            let result = subscribe_provider
                .acquire_subscription(
                    &subscribe_pubkey,
                    SubscriptionReason::DirectAccount,
                )
                .await;
            let _ = result_tx.send(result);
        });

        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                &mut result_rx,
            )
            .await
            .is_err(),
            "same-pubkey subscribe must wait while defensive eviction holds the per-pubkey subscription lock"
        );

        release_eviction.notify_one();
        assert!(eviction_task.await.unwrap());
        let subscribe_result = tokio::time::timeout(
            Duration::from_secs(1),
            &mut result_rx,
        )
        .await
        .expect("subscription should complete after eviction releases the lock")
        .expect("subscription task should send its result");
        subscribe_task.await.unwrap();

        assert!(subscribe_result.is_ok());
        assert!(remote_account_provider.is_watching(&pubkey));
    }
}
