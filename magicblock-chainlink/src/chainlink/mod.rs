use std::{
    path::Path,
    sync::{Arc, atomic::AtomicU64},
};

use dlp_api::pda::ephemeral_balance_pda_from_payer;
use engine::Engine;
use errors::{ChainlinkError, ChainlinkResult};
use fetch_cloner::FetchCloner;
use magicblock_aml::RiskService;
use magicblock_config::config::ChainLinkConfig;
use magicblock_metrics::metrics::AccountFetchContext;
use nucleus::runtime::TransactionView;
use solana_account::{AccountMode, AccountSharedData, ReadableAccount};
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use tokio::{
    sync::{broadcast, mpsc},
    task::{self, JoinSet},
};
use tracing::*;

use crate::{
    cloner,
    config::ChainlinkConfig,
    fetch_cloner::FetchAndCloneResult,
    filters::is_noop_system_transfer,
    remote_account_provider::{
        ChainPubsubClient, ChainRpcClient, ChainRpcClientImpl, Endpoints,
        RemoteAccountProvider, chain_updates_client::ChainUpdatesClient,
    },
    submux::SubMuxClient,
};

mod account_still_undelegating_on_chain;
pub mod config;
pub mod errors;
pub mod fetch_cloner;

pub(crate) const SUBSCRIPTION_UPDATE_LIMIT: usize = 5_000;

/// Production Chainlink stack.
pub type ProdInnerChainlink =
    InnerChainlink<ChainRpcClientImpl, SubMuxClient<ChainUpdatesClient>>;

/// Production replication-aware Chainlink stack.
pub type ProdChainlink = ReplicationModeAwareChainlink<
    ChainRpcClientImpl,
    SubMuxClient<ChainUpdatesClient>,
>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedUndelegationRequest {
    pub request_pda: Pubkey,
    pub delegated_account: Pubkey,
    pub expires_at_slot: u64,
    pub observed_slot: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AccountStatusOnEr {
    /// The account is missing from the ER bank, so its ER delegation state is unknown.
    #[default]
    Missing,
    /// The account is present on ER and represented as delegated.
    Delegated,
    /// The account is present on ER and is not represented as delegated.
    NotDelegated,
}

impl AccountStatusOnEr {
    pub fn is_delegated(&self) -> bool {
        matches!(self, Self::Delegated)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Delegated => "delegated",
            Self::NotDelegated => "not_delegated",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AccountDelegationStatus {
    pub delegated_on_base: bool,
    pub account_on_er: AccountStatusOnEr,
}

impl AccountDelegationStatus {
    #[deprecated(
        note = "use AccountDelegationStatus directly; this bool treats missing-on-ER as not delegated"
    )]
    pub fn delegated_on_base_and_er(&self) -> bool {
        self.delegated_on_base && self.account_on_er.is_delegated()
    }

    pub fn not_ready_reason(&self) -> Option<&'static str> {
        #[allow(deprecated)]
        let delegated_on_base_and_er = self.delegated_on_base_and_er();
        if delegated_on_base_and_er {
            None
        } else if !self.delegated_on_base {
            Some("not_delegated_on_base")
        } else {
            Some(match self.account_on_er {
                AccountStatusOnEr::Missing => "delegated_on_base_missing_on_er",
                AccountStatusOnEr::Delegated => {
                    "delegated_on_base_and_er_mismatch"
                }
                AccountStatusOnEr::NotDelegated => {
                    "delegated_on_base_not_delegated_on_er"
                }
            })
        }
    }
}

// -----------------
// Chainlink
// -----------------
pub struct InnerChainlink<T: ChainRpcClient, U: ChainPubsubClient> {
    engine: Engine,
    fetch_cloner: Option<Arc<FetchCloner<T, U>>>,
    undelegation_request_sender: broadcast::Sender<ObservedUndelegationRequest>,
    /// Removes readonly accounts whose remote subscription became unusable.
    #[allow(unused)]
    stale_accounts_sub: Option<task::JoinHandle<()>>,
    /// Unsubscribes and removes readonly accounts evicted by the engine cache.
    #[allow(unused)]
    evicted_accounts_sub: Option<task::JoinHandle<()>>,
}

pub enum ReplicationModeAwareChainlink<T: ChainRpcClient, U: ChainPubsubClient>
{
    Enabled(InnerChainlink<T, U>),
    Disabled,
}

impl<T: ChainRpcClient, U: ChainPubsubClient>
    ReplicationModeAwareChainlink<T, U>
{
    pub fn enabled(chainlink: InnerChainlink<T, U>) -> Self {
        Self::Enabled(chainlink)
    }

    pub fn disabled() -> ChainlinkResult<Self> {
        Ok(Self::Disabled)
    }

    pub async fn ensure_accounts(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        fetch_context: impl Into<AccountFetchContext>,
    ) -> ChainlinkResult<()> {
        let fetch_context = fetch_context.into();
        match self {
            Self::Enabled(chainlink) => {
                chainlink
                    .ensure_accounts(
                        pubkeys,
                        mark_empty_if_not_found,
                        fetch_context,
                    )
                    .await
            }
            Self::Disabled => Ok(()),
        }
    }

    pub async fn ensure_transaction_accounts(
        &self,
        tx: &TransactionView,
    ) -> ChainlinkResult<()> {
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
        fetch_context: impl Into<AccountFetchContext>,
    ) -> ChainlinkResult<Vec<Option<AccountSharedData>>> {
        let fetch_context = fetch_context.into();
        match self {
            Self::Enabled(chainlink) => {
                chainlink.fetch_accounts(pubkeys, fetch_context).await
            }
            Self::Disabled => Ok(vec![None; pubkeys.len()]),
        }
    }

    #[deprecated(
        note = "use AccountDelegationStatus directly; this bool treats missing-on-ER as not delegated"
    )]
    pub async fn accounts_delegated_on_base_and_er(
        &self,
        pubkeys: &[Pubkey],
        fetch_context: impl Into<AccountFetchContext>,
    ) -> ChainlinkResult<Vec<bool>> {
        Ok(self
            .account_delegation_statuses(pubkeys, fetch_context)
            .await?
            .into_iter()
            .map(|status| {
                #[allow(deprecated)]
                let delegated_on_base_and_er =
                    status.delegated_on_base_and_er();
                delegated_on_base_and_er
            })
            .collect())
    }

    pub async fn account_delegation_statuses(
        &self,
        pubkeys: &[Pubkey],
        fetch_context: impl Into<AccountFetchContext>,
    ) -> ChainlinkResult<Vec<AccountDelegationStatus>> {
        let fetch_context = fetch_context.into();
        match self {
            Self::Enabled(chainlink) => {
                chainlink
                    .account_delegation_statuses(pubkeys, fetch_context)
                    .await
            }
            Self::Disabled => {
                Ok(vec![AccountDelegationStatus::default(); pubkeys.len()])
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
            Self::Disabled => Ok(()),
        }
    }

    pub async fn fetch_undelegation_requests(
        &self,
    ) -> ChainlinkResult<Vec<ObservedUndelegationRequest>> {
        match self {
            Self::Enabled(chainlink) => {
                chainlink.fetch_undelegation_requests().await
            }
            Self::Disabled => Ok(Vec::new()),
        }
    }

    pub fn fetch_count(&self) -> Option<u64> {
        match self {
            Self::Enabled(chainlink) => chainlink.fetch_count(),
            Self::Disabled => None,
        }
    }

    pub fn fetch_cloner(&self) -> Option<&Arc<FetchCloner<T, U>>> {
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

    pub fn subscribe_undelegation_requests(
        &self,
    ) -> Option<broadcast::Receiver<ObservedUndelegationRequest>> {
        match self {
            Self::Enabled(chainlink) => {
                Some(chainlink.subscribe_undelegation_requests())
            }
            Self::Disabled => None,
        }
    }
}

impl<T: ChainRpcClient, U: ChainPubsubClient> InnerChainlink<T, U> {
    fn get_account(&self, pubkey: &Pubkey) -> Option<AccountSharedData> {
        self.engine.accounts().get(pubkey).ok().flatten()
    }

    pub fn try_new(
        engine: Engine,
        fetch_cloner: Option<Arc<FetchCloner<T, U>>>,
    ) -> ChainlinkResult<Self> {
        let (undelegation_request_sender, _) = broadcast::channel(1024);
        Self::try_new_with_undelegation_request_sender(
            engine,
            fetch_cloner,
            undelegation_request_sender,
        )
    }

    pub fn try_new_with_undelegation_request_sender(
        engine: Engine,
        fetch_cloner: Option<Arc<FetchCloner<T, U>>>,
        undelegation_request_sender: broadcast::Sender<
            ObservedUndelegationRequest,
        >,
    ) -> ChainlinkResult<Self> {
        let (stale_accounts_sub, evicted_accounts_sub) =
            if let Some(fetch_cloner) = &fetch_cloner {
                let stale_accounts_rx =
                    fetch_cloner.try_get_stale_account_rx()?;
                (
                    Some(Self::subscribe_stale_accounts(
                        engine.clone(),
                        fetch_cloner.remote_account_provider(),
                        stale_accounts_rx,
                    )),
                    Some(Self::subscribe_account_evictions(
                        engine.clone(),
                        fetch_cloner.remote_account_provider(),
                        engine.accounts().subscribe_evictions(),
                    )),
                )
            } else {
                (None, None)
            };
        Ok(Self {
            engine,
            fetch_cloner,
            undelegation_request_sender,
            stale_accounts_sub,
            evicted_accounts_sub,
        })
    }

    #[allow(clippy::too_many_arguments)]
    #[instrument(skip(endpoints, engine, config, chainlink_config,))]
    pub async fn try_new_from_endpoints(
        endpoints: &Endpoints,
        commitment: CommitmentConfig,
        engine: Engine,
        validator_keypair: Keypair,
        config: ChainlinkConfig,
        chainlink_config: &ChainLinkConfig,
        ledger_path: &Path,
        chain_slot: Arc<AtomicU64>,
    ) -> ChainlinkResult<ProdInnerChainlink> {
        // Extract accounts provider and create fetch cloner while connecting
        // the subscription channel
        let (tx, rx) = tokio::sync::mpsc::channel(SUBSCRIPTION_UPDATE_LIMIT);
        let account_provider = RemoteAccountProvider::try_from_urls_and_config(
            endpoints,
            commitment,
            tx,
            &config.remote_account_provider,
            Some(chain_slot),
        )
        .await?;
        let (undelegation_request_sender, _) = broadcast::channel(1024);
        let fetch_cloner = if let Some(provider) = account_provider {
            let provider = Arc::new(provider);
            let risk_service = RiskService::try_from_config(
                &chainlink_config.risk,
                ledger_path,
            )?
            .map(Arc::new);
            let fetch_cloner =
                FetchCloner::new_with_undelegation_request_sender(
                    &provider,
                    engine.clone(),
                    validator_keypair,
                    rx,
                    chainlink_config.allowed_programs.clone(),
                    risk_service,
                    undelegation_request_sender.clone(),
                );
            Some(fetch_cloner)
        } else {
            None
        };

        InnerChainlink::try_new_with_undelegation_request_sender(
            engine,
            fetch_cloner,
            undelegation_request_sender,
        )
    }

    fn subscribe_stale_accounts(
        engine: Engine,
        remote_account_provider: &Arc<RemoteAccountProvider<T, U>>,
        mut stale_accounts_rx: mpsc::Receiver<Pubkey>,
    ) -> task::JoinHandle<()> {
        let remote_account_provider = remote_account_provider.clone();

        task::spawn(async move {
            while let Some(pubkey) = stale_accounts_rx.recv().await {
                // Removal notifications can race with a new acquire_subscription for the same
                // pubkey. The provider helper holds the same per-pubkey subscription lock used
                // by acquire/release while it re-checks is_watching and submits eviction. This
                // prevents an EvictAccount transaction from being submitted after a fresh
                // subscription has made the account watched again, without blocking unrelated
                // pubkeys on the defensive-eviction slow path.
                let engine = engine.clone();
                let evicted = remote_account_provider
                    .evict_unwatched_with_subscription_lock(&pubkey, || async move {
                        // MagicRoot is the authoritative deletion boundary and
                        // rejects the request if the account became mutable.
                        trace!(
                            pubkey = %pubkey,
                            "Submitting eviction transaction for unwatched account"
                        );
                        if let Err(err) = cloner::evict_account(&engine, pubkey).await {
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
            warn!("Stale accounts channel closed");
        })
    }

    fn subscribe_account_evictions(
        engine: Engine,
        remote_account_provider: &Arc<RemoteAccountProvider<T, U>>,
        mut evictions: broadcast::Receiver<Pubkey>,
    ) -> task::JoinHandle<()> {
        let remote_account_provider = remote_account_provider.clone();

        task::spawn(async move {
            let mut pending = JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    event = evictions.recv() => {
                        let pubkey = match event {
                            Ok(pubkey) => pubkey,
                            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                                warn!(skipped, "Lagged behind engine account evictions");
                                continue;
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                        };
                        let engine = engine.clone();
                        let remote_account_provider = remote_account_provider.clone();
                        pending.spawn(async move {
                            // Engine emits only cache-tracked readonly accounts;
                            // MagicRoot still rejects a later mutable transition.
                            if let Err(err) = remote_account_provider.unsubscribe(&pubkey).await {
                                warn!(
                                    pubkey = %pubkey,
                                    error = ?err,
                                    "Failed to unsubscribe engine-evicted account"
                                );
                                return;
                            }
                            if let Err(err) = cloner::evict_account(&engine, pubkey).await {
                                warn!(
                                    pubkey = %pubkey,
                                    error = ?err,
                                    "Failed to remove engine-evicted account"
                                );
                            }
                        });
                    }
                    Some(result) = pending.join_next(), if !pending.is_empty() => {
                        if let Err(err) = result {
                            warn!(error = ?err, "Engine account eviction task failed");
                        }
                    }
                }
            }
        })
    }

    /// Ensures that all accounts required by the transaction exist on chain,
    /// are delegated to our validator if writable and that their latest state
    /// is cloned in our validator.
    #[instrument(skip(self, tx))]
    pub async fn ensure_transaction_accounts(
        &self,
        tx: &TransactionView,
    ) -> ChainlinkResult<()> {
        let signature = tx.signatures()[0];
        if is_noop_system_transfer(tx) {
            trace!(
                tx_sig = %signature,
                "Skipping account ensure for noop system transfer transaction"
            );
            return Ok(());
        }

        let mut pubkeys = tx.static_account_keys().to_vec();
        let feepayer = &tx.static_account_keys()[0];

        let balance_pda = ephemeral_balance_pda_from_payer(feepayer, 0);

        // Determine if we need to clone the escrow account for the feepayer
        let clone_escrow = self.get_account(&balance_pda).is_none();

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
        self.ensure_accounts(
            &pubkeys,
            mark_empty_if_not_found,
            AccountFetchContext::send_transaction(signature),
        )
        .await
    }

    /// Same as fetch accounts, but does not return the accounts, just
    /// ensures were cloned into our validator if they exist on chain.
    /// If we're offline and not syncing accounts then this is a no-op.
    #[instrument(skip(self, pubkeys, mark_empty_if_not_found, fetch_context))]
    pub async fn ensure_accounts(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        fetch_context: impl Into<AccountFetchContext>,
    ) -> ChainlinkResult<()> {
        let fetch_context = fetch_context.into();
        let Some(fetch_cloner) = self.fetch_cloner() else {
            return Ok(());
        };
        self.fetch_accounts_common(
            fetch_cloner,
            pubkeys,
            mark_empty_if_not_found,
            fetch_context,
        )
        .await?;
        Ok(())
    }

    /// Fetches the accounts from the bank if we're offline and not syncing accounts.
    /// Otherwise ensures that the accounts exist on chain and were cloned into our validator
    /// and returns their state from the bank (which may be None if the account does not
    /// exist locally or on chain).
    #[instrument(skip(self, pubkeys, fetch_context))]
    pub async fn fetch_accounts(
        &self,
        pubkeys: &[Pubkey],
        fetch_context: impl Into<AccountFetchContext>,
    ) -> ChainlinkResult<Vec<Option<AccountSharedData>>> {
        let fetch_context = fetch_context.into();
        if tracing::enabled!(tracing::Level::TRACE) {
            let count = pubkeys.len();
            trace!(count, "Fetching accounts");
        }
        let Some(fetch_cloner) = self.fetch_cloner() else {
            // If we're offline and not syncing accounts then we just get them from the bank
            let accessor = self.engine.accounts();
            let loader = accessor.loader();
            return Ok(pubkeys
                .iter()
                .map(|pubkey| loader.load(pubkey).ok().flatten())
                .collect());
        };
        let _ = self
            .fetch_accounts_common(fetch_cloner, pubkeys, None, fetch_context)
            .await?;

        let accessor = self.engine.accounts();
        let loader = accessor.loader();
        let accounts = pubkeys
            .iter()
            .map(|pubkey| loader.load(pubkey).ok().flatten())
            .collect();
        Ok(accounts)
    }

    #[instrument(skip(self, pubkeys, fetch_context))]
    #[deprecated(
        note = "use AccountDelegationStatus directly; this bool treats missing-on-ER as not delegated"
    )]
    pub async fn accounts_delegated_on_base_and_er(
        &self,
        pubkeys: &[Pubkey],
        fetch_context: impl Into<AccountFetchContext>,
    ) -> ChainlinkResult<Vec<bool>> {
        Ok(self
            .account_delegation_statuses(pubkeys, fetch_context)
            .await?
            .into_iter()
            .map(|status| {
                #[allow(deprecated)]
                let delegated_on_base_and_er =
                    status.delegated_on_base_and_er();
                delegated_on_base_and_er
            })
            .collect())
    }

    #[instrument(skip(self, pubkeys, fetch_context))]
    pub async fn account_delegation_statuses(
        &self,
        pubkeys: &[Pubkey],
        fetch_context: impl Into<AccountFetchContext>,
    ) -> ChainlinkResult<Vec<AccountDelegationStatus>> {
        let fetch_context = fetch_context.into();
        let Some(fetch_cloner) = self.fetch_cloner() else {
            return Ok(vec![AccountDelegationStatus::default(); pubkeys.len()]);
        };
        let remote_accounts = fetch_cloner
            .fetch_remote_accounts(pubkeys, fetch_context)
            .await?;
        if remote_accounts.len() != pubkeys.len() {
            return Err(ChainlinkError::UnexpectedAccountCount(format!(
                "expected {} remote accounts, got {}",
                pubkeys.len(),
                remote_accounts.len()
            )));
        }

        let accessor = self.engine.accounts();
        let loader = accessor.loader();
        Ok(pubkeys
            .iter()
            .zip(remote_accounts)
            .map(|(pubkey, remote_account)| {
                let delegated_on_base =
                    remote_account.is_owned_by_delegation_program();
                let account_on_er = match loader.load(pubkey).ok().flatten() {
                    None => AccountStatusOnEr::Missing,
                    Some(account) => {
                        // Q: do we need to compare the owner? isn't delegated() alone enough?
                        if account.is(AccountMode::Delegated)
                            || account.owner().eq(&dlp_api::id())
                        {
                            AccountStatusOnEr::Delegated
                        } else {
                            AccountStatusOnEr::NotDelegated
                        }
                    }
                };
                AccountDelegationStatus {
                    delegated_on_base,
                    account_on_er,
                }
            })
            .collect())
    }

    #[instrument(skip(self, fetch_cloner, pubkeys, mark_empty_if_not_found))]
    async fn fetch_accounts_common(
        &self,
        fetch_cloner: &FetchCloner<T, U>,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        if tracing::enabled!(tracing::Level::TRACE) {
            let count = pubkeys.len();
            let mark_empty_count = mark_empty_if_not_found.map(|k| k.len());
            trace!(count, mark_empty_count, "Fetching accounts");
        }
        // If any of the accounts was invalid and couldn't be fetched/cloned then
        // we return an error.
        let result = fetch_cloner
            .fetch_and_clone_accounts_with_dedup(
                pubkeys,
                mark_empty_if_not_found,
                None,
                fetch_context,
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

    pub async fn fetch_undelegation_requests(
        &self,
    ) -> ChainlinkResult<Vec<ObservedUndelegationRequest>> {
        let Some(fetch_cloner) = self.fetch_cloner() else {
            return Ok(Vec::new());
        };
        fetch_cloner.fetch_undelegation_requests().await
    }

    pub fn fetch_cloner(&self) -> Option<&Arc<FetchCloner<T, U>>> {
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

    pub fn subscribe_undelegation_requests(
        &self,
    ) -> broadcast::Receiver<ObservedUndelegationRequest> {
        self.undelegation_request_sender.subscribe()
    }
}

// -----------------
// Helper Functions
// -----------------

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use magicblock_metrics::metrics::AccountFetchContext;
    use nucleus::testkit::signed_view;
    use solana_hash::Hash;
    use solana_keypair::Keypair;
    use solana_pubkey::Pubkey;
    use tokio::sync::mpsc;

    use super::{ReplicationModeAwareChainlink, errors::ChainlinkError};
    use crate::{
        remote_account_provider::{
            SubscriptionReason,
            chain_pubsub_client::mock::ChainPubsubClientMock,
        },
        testing::{init_logger, rpc_client_mock::ChainRpcClientMock},
    };

    type TestReplicationModeAwareChainlink = ReplicationModeAwareChainlink<
        ChainRpcClientMock,
        ChainPubsubClientMock,
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
                RemoteAccountProvider, chain_slot::ChainSlot,
            },
            testing::{
                rpc_client_mock::ChainRpcClientMockBuilder,
                utils::create_test_subscribed_accounts,
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
        let (subscribed_accounts, config) = create_test_subscribed_accounts();
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

    fn disabled_chainlink() -> TestReplicationModeAwareChainlink {
        TestReplicationModeAwareChainlink::disabled()
            .expect("disabled Chainlink should be constructed")
    }

    #[tokio::test]
    async fn disabled_mode_ensure_accounts_is_noop() {
        let chainlink = disabled_chainlink();
        let pubkey = Pubkey::new_unique();

        let result = chainlink
            .ensure_accounts(
                &[pubkey],
                None,
                AccountFetchContext::rpc_get_account(),
            )
            .await;

        result.expect("disabled ensure_accounts should succeed");
        assert_eq!(chainlink.fetch_count(), None);
        assert!(!chainlink.is_watching(&pubkey));
    }

    #[tokio::test]
    async fn disabled_mode_fetch_accounts_returns_none_values() {
        let chainlink = disabled_chainlink();
        let pubkeys = vec![Pubkey::new_unique(), Pubkey::new_unique()];

        let result = chainlink
            .fetch_accounts(
                &pubkeys,
                AccountFetchContext::rpc_get_multiple_accounts(),
            )
            .await;

        let accounts = result.expect("disabled fetch_accounts should succeed");
        assert_eq!(accounts.len(), 2);
        assert!(accounts.iter().all(Option::is_none));
    }

    #[tokio::test]
    async fn disabled_mode_rejects_transaction_ensure() {
        let chainlink = disabled_chainlink();
        let (_, sanitized_tx) =
            signed_view(&Keypair::new(), &[], Hash::default());

        let error = chainlink
            .ensure_transaction_accounts(&sanitized_tx)
            .await
            .expect_err("disabled transaction ensure should be rejected");

        assert!(matches!(error, ChainlinkError::DisabledForNonPrimaryMode));
    }

    #[tokio::test]
    async fn test_defensive_eviction_blocks_same_pubkey_subscription_until_eviction_finishes()
     {
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
            tokio::time::timeout(Duration::from_millis(50), &mut result_rx,)
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
