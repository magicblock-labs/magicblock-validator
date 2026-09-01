use std::{
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};

use engine::Engine;
use errors::{ChainlinkError, ChainlinkResult};
use fetch_cloner::FetchCloner;
use keeper::error::KeeperError;
use magicblock_aml::RiskService;
use magicblock_config::config::ChainLinkConfig;
use magicblock_core::token_programs::{
    is_ata, try_derive_eata_address_and_bump,
};
use magicblock_metrics::metrics::{
    AccountFetchContext, AccountFetchEntrypoint,
};
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
    remote_account_provider::{
        ChainPubsubClient, ChainRpcClient, ChainRpcClientImpl, Endpoints,
        RemoteAccountProvider, SubscriptionReason,
        chain_updates_client::ChainUpdatesClient,
    },
    submux::SubMuxClient,
};

mod account_still_undelegating_on_chain;
pub mod config;
pub mod errors;
pub mod fetch_cloner;

pub(crate) const SUBSCRIPTION_UPDATE_LIMIT: usize = 5_000;
const ENSURE_ACCOUNTS_TIMEOUT: Duration = Duration::from_secs(30);

/// Production Chainlink stack.
pub type ProdChainlink =
    InnerChainlink<ChainRpcClientImpl, SubMuxClient<ChainUpdatesClient>>;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountDelegationSession {
    pub locally_protected: bool,
    pub remote_slot: u64,
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

impl<T: ChainRpcClient, U: ChainPubsubClient> InnerChainlink<T, U> {
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
                        engine.accounts().subscribe_evictions()?,
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
        chain_slot: Arc<AtomicU64>,
    ) -> ChainlinkResult<ProdChainlink> {
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
            let risk_service =
                RiskService::try_from_config(&chainlink_config.risk)?
                    .map(Arc::new);
            match risk_service.as_ref() {
                // Which policy is live decides whether an action can activate
                // unchecked, so make it visible at startup.
                Some(service) => info!(
                    risk_server_url = %chainlink_config.risk.risk_server_url,
                    check_strategy = ?service.check_strategy(),
                    "Address risk checks enabled"
                ),
                None => info!("Address risk checks disabled"),
            }
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
                let subscription = remote_account_provider
                    .lock_account_eviction(&pubkey)
                    .await;
                if subscription.is_watching() {
                    trace!(
                        pubkey = %pubkey,
                        "Skipping removal notification because account is watched again"
                    );
                    continue;
                }
                let mut accessor =
                    match cloner::claim_account_eviction(&engine, pubkey).await
                    {
                        Ok(Some(accessor)) => accessor,
                        Ok(None) => continue,
                        Err(err) => {
                            warn!(
                                pubkey = %pubkey,
                                error = ?err,
                                "Failed to claim unwatched account eviction"
                            );
                            continue;
                        }
                    };
                trace!(
                    pubkey = %pubkey,
                    "Submitting eviction transaction for unwatched account"
                );
                if let Err(err) = subscription.unsubscribe().await {
                    warn!(
                        pubkey = %pubkey,
                        error = ?err,
                        "Failed to unsubscribe unwatched account"
                    );
                    continue;
                }
                if let Err(err) =
                    cloner::delete_claimed_account(&mut accessor, pubkey).await
                {
                    warn!(
                        pubkey = %pubkey,
                        error = ?err,
                        "Failed to submit eviction transaction"
                    );
                }
            }
            warn!("Stale accounts channel closed");
        })
    }

    fn subscribe_account_evictions(
        engine: Engine,
        remote_account_provider: &Arc<RemoteAccountProvider<T, U>>,
        mut evictions: mpsc::Receiver<Pubkey>,
    ) -> task::JoinHandle<()> {
        let remote_account_provider = remote_account_provider.clone();

        task::spawn(async move {
            let mut pending = JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    event = evictions.recv() => {
                        let pubkey = match event {
                            Some(pubkey) => pubkey,
                            None => break,
                        };
                        let engine = engine.clone();
                        let remote_account_provider = remote_account_provider.clone();
                        pending.spawn(async move {
                            let subscription = remote_account_provider
                                .lock_account_eviction(&pubkey)
                                .await;
                            let (mut accessor, ata_info) = match cloner::claim_cached_account_eviction(
                                &engine,
                                pubkey,
                                |account| {
                                    is_ata(
                                        &pubkey,
                                        *account.owner(),
                                        account.data(),
                                    )
                                },
                            ).await {
                                Ok(Some(claim)) => claim,
                                Ok(None) => return,
                                Err(err) => {
                                    warn!(
                                        pubkey = %pubkey,
                                        error = ?err,
                                        "Failed to claim engine-evicted account"
                                    );
                                    return;
                                }
                            };
                            if let Err(err) = subscription.unsubscribe().await {
                                warn!(
                                    pubkey = %pubkey,
                                    error = ?err,
                                    "Failed to unsubscribe engine-evicted account"
                                );
                                return;
                            }
                            if let Err(err) = cloner::delete_claimed_account(
                                &mut accessor,
                                pubkey,
                            )
                            .await
                            {
                                warn!(
                                    pubkey = %pubkey,
                                    error = ?err,
                                    "Failed to remove engine-evicted account"
                                );
                                return;
                            }
                            drop(subscription);
                            let Some(ata_info) = ata_info else {
                                return;
                            };
                            let Some((eata_pubkey, _)) =
                                try_derive_eata_address_and_bump(
                                    &ata_info.owner,
                                    &ata_info.mint,
                                )
                            else {
                                return;
                            };
                            if let Err(err) = remote_account_provider
                                .release_single_subscription(
                                    &eata_pubkey,
                                    SubscriptionReason::AtaProjection,
                                )
                                .await
                            {
                                warn!(
                                    pubkey = %pubkey,
                                    eata_pubkey = %eata_pubkey,
                                    error = ?err,
                                    "Failed to release eATA projection subscription for evicted ATA"
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

    /// Ensures requested accounts are materialized locally. Missing remote
    /// accounts are represented as placeholders.
    /// Returns the number of requested remote accounts claimed by this call.
    /// If we're offline and not syncing accounts then this is a no-op.
    pub async fn ensure_accounts(
        &self,
        pubkeys: &[Pubkey],
        fetch_origin: AccountFetchEntrypoint,
    ) -> ChainlinkResult<u64> {
        let Some(fetch_cloner) = self.fetch_cloner() else {
            return Ok(0);
        };

        let pending = {
            let accessor = self.engine.accounts();
            let loader = accessor.loader();
            let mut pending = None;
            for pubkey in pubkeys {
                let mode = loader
                    .read(pubkey, |account| account.mode())
                    .map_err(KeeperError::from)?;
                if mode.is_none_or(|mode| mode == AccountMode::Transient) {
                    pending
                        .get_or_insert_with(|| {
                            Vec::with_capacity(pubkeys.len())
                        })
                        .push(*pubkey);
                }
            }
            pending
        };
        let Some(pending) = pending else {
            return Ok(0);
        };

        tokio::time::timeout(
            ENSURE_ACCOUNTS_TIMEOUT,
            fetch_cloner
                .fetch_and_clone_requested_accounts(&pending, fetch_origin),
        )
        .await
        .unwrap_or_else(|_| {
            Err(ChainlinkError::EnsureAccountsTimeout(
                ENSURE_ACCOUNTS_TIMEOUT.as_secs(),
            ))
        })
    }

    /// Fetches the accounts from the bank if we're offline and not syncing accounts.
    /// Otherwise materializes requested accounts locally, using placeholders
    /// for accounts missing on chain, and returns their state from the bank.
    pub async fn fetch_accounts(
        &self,
        pubkeys: &[Pubkey],
        fetch_origin: AccountFetchEntrypoint,
    ) -> ChainlinkResult<Vec<Option<AccountSharedData>>> {
        if tracing::enabled!(tracing::Level::TRACE) {
            let count = pubkeys.len();
            trace!(count, "Fetching accounts");
        }
        let snapshot = |account: &AccountSharedData| {
            AccountSharedData::from(account.owned())
        };
        self.ensure_accounts(pubkeys, fetch_origin).await?;

        let accessor = self.engine.accounts();
        let loader = accessor.loader();
        let accounts = pubkeys
            .iter()
            .map(|pubkey| loader.read(pubkey, snapshot).ok().flatten())
            .collect();
        Ok(accounts)
    }

    /// Ensures accounts are materialized, then projects the local delegation
    /// session metadata needed to validate durable intent recovery.
    pub async fn account_delegation_sessions(
        &self,
        pubkeys: &[Pubkey],
        fetch_origin: AccountFetchEntrypoint,
    ) -> ChainlinkResult<Vec<Option<AccountDelegationSession>>> {
        self.ensure_accounts(pubkeys, fetch_origin).await?;

        let accessor = self.engine.accounts();
        let loader = accessor.loader();
        Ok(pubkeys
            .iter()
            .map(|pubkey| {
                loader
                    .read(pubkey, |account| AccountDelegationSession {
                        locally_protected: account.is(AccountMode::Delegated)
                            || account.is(AccountMode::Transient),
                        remote_slot: account.slot(),
                    })
                    .ok()
                    .flatten()
            })
            .collect())
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
                let account_on_er = match loader
                    .read(pubkey, |account| {
                        account.is(AccountMode::Delegated)
                            || account.owner().eq(&dlp_api::id())
                    })
                    .ok()
                    .flatten()
                {
                    None => AccountStatusOnEr::Missing,
                    Some(true) => AccountStatusOnEr::Delegated,
                    Some(false) => AccountStatusOnEr::NotDelegated,
                };
                AccountDelegationStatus {
                    delegated_on_base,
                    account_on_er,
                }
            })
            .collect())
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

    use solana_pubkey::Pubkey;
    use tokio::sync::mpsc;

    use crate::{
        remote_account_provider::{
            SubscriptionReason,
            chain_pubsub_client::mock::ChainPubsubClientMock,
        },
        testing::{init_logger, rpc_client_mock::ChainRpcClientMock},
    };

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

    /// Proves the subscription boundary remains held until same-pubkey account
    /// eviction work finishes.
    #[tokio::test]
    async fn test_account_eviction_blocks_same_pubkey_subscription_until_eviction_finishes()
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
            let _eviction = eviction_provider
                .lock_account_eviction(&eviction_pubkey)
                .await;
            eviction_started_for_task.notify_one();
            release_eviction_for_task.notified().await;
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
            "same-pubkey subscribe must wait while account eviction holds the subscription lock"
        );

        release_eviction.notify_one();
        eviction_task.await.unwrap();
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
