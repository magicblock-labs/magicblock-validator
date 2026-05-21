use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
};

use dlp_api::pda::ephemeral_balance_pda_from_payer;
use errors::{ChainlinkError, ChainlinkResult};
use fetch_cloner::FetchCloner;
use magicblock_accounts_db::{traits::AccountsBank, AccountsDbResult};
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
use tokio::{
    sync::{mpsc, Mutex as AsyncMutex},
    task,
};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryEnableOutcome {
    RuntimeActive,
    DisabledByConfig,
}

impl PrimaryEnableOutcome {
    pub fn is_active(self) -> bool {
        matches!(self, Self::RuntimeActive)
    }
}

#[deprecated(note = "use PrimaryEnableOutcome")]
pub type ChainlinkPrimaryEnablement = PrimaryEnableOutcome;

#[async_trait::async_trait]
pub trait PrimaryChainlink: Send + Sync {
    fn reset_accounts_bank(&self) -> AccountsDbResult<()>;
    async fn enable_primary(&self) -> ChainlinkResult<PrimaryEnableOutcome>;
    async fn disable(&self) -> ChainlinkResult<()>;
    async fn primary_runtime_readiness(&self) -> PrimaryRuntimeReadiness;
}

// -----------------
// Chainlink
// -----------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainlinkPrimaryReadiness {
    /// Runtime is enabled and available for primary Chainlink ensure paths.
    Ready,
    /// Local-only readiness: Chainlink is disabled because this instance has
    /// no remote provider configured or remote-account-provider lifecycle is
    /// disabled by config, so aperture ensure gates may proceed without a
    /// runtime.
    ReadyWithoutRemoteProvider,
    /// Primary Chainlink ensure paths must not run yet.
    NotReady,
}

impl ChainlinkPrimaryReadiness {
    pub fn allows_aperture_ensure(self) -> bool {
        matches!(self, Self::Ready | Self::ReadyWithoutRemoteProvider)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::ReadyWithoutRemoteProvider => "ready_without_remote_provider",
            Self::NotReady => "not_ready",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryRuntimeReadiness {
    Ready,
    DisabledByConfig,
    NotReady,
}

impl PrimaryRuntimeReadiness {
    pub fn allows_primary_ensure(self) -> bool {
        matches!(self, Self::Ready | Self::DisabledByConfig)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChainlinkLifecycleState {
    Disabled = 0,
    Starting = 1,
    Enabled = 2,
    Stopping = 3,
}

impl ChainlinkLifecycleState {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Starting,
            2 => Self::Enabled,
            3 => Self::Stopping,
            _ => Self::Disabled,
        }
    }
}

struct ChainlinkRuntime<T, U, V, C>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    fetch_cloner: Arc<FetchCloner<T, U, V, C>>,
    /// The subscription to events for each account that is removed from
    /// the accounts tracked by the provider.
    /// In that case we also remove it from the bank since it is no longer
    /// synchronized.
    #[allow(unused)] // needed to cleanup chainlink
    removed_accounts_sub: Option<task::JoinHandle<()>>,
}

#[allow(dead_code)]
struct ChainlinkRuntimeBuildConfig<C>
where
    C: Cloner,
{
    endpoints: Endpoints,
    commitment: CommitmentConfig,
    cloner: Arc<C>,
    validator_keypair_bytes: [u8; 64],
    chainlink_config: ChainLinkConfig,
    runtime_config: ChainlinkConfig,
    ledger_path: PathBuf,
}

pub struct Chainlink<
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
> {
    accounts_bank: Arc<V>,
    runtime: AsyncMutex<Option<ChainlinkRuntime<T, U, V, C>>>,
    lifecycle_state: AtomicU8,
    #[allow(dead_code)]
    runtime_build_config: Option<ChainlinkRuntimeBuildConfig<C>>,

    validator_id: Pubkey,

    /// If true, remove confined accounts during bank reset
    remove_confined_accounts: bool,
}

impl<T: ChainRpcClient, U: ChainPubsubClient, V: AccountsBank, C: Cloner>
    Chainlink<T, U, V, C>
{
    pub async fn primary_runtime_readiness(&self) -> PrimaryRuntimeReadiness {
        if self.runtime.lock().await.is_some() {
            return PrimaryRuntimeReadiness::Ready;
        }

        let Some(build) = self.runtime_build_config.as_ref() else {
            return PrimaryRuntimeReadiness::DisabledByConfig;
        };
        if !build
            .runtime_config
            .remote_account_provider
            .lifecycle_mode()
            .needs_remote_account_provider()
        {
            PrimaryRuntimeReadiness::DisabledByConfig
        } else {
            PrimaryRuntimeReadiness::NotReady
        }
    }

    pub async fn primary_readiness(&self) -> ChainlinkPrimaryReadiness {
        match ChainlinkLifecycleState::from_u8(
            self.lifecycle_state.load(Ordering::SeqCst),
        ) {
            ChainlinkLifecycleState::Enabled => {
                if self.runtime.lock().await.is_some() {
                    ChainlinkPrimaryReadiness::Ready
                } else {
                    ChainlinkPrimaryReadiness::NotReady
                }
            }
            ChainlinkLifecycleState::Starting
            | ChainlinkLifecycleState::Stopping => {
                ChainlinkPrimaryReadiness::NotReady
            }
            ChainlinkLifecycleState::Disabled => {
                let Some(build) = self.runtime_build_config.as_ref() else {
                    return ChainlinkPrimaryReadiness::ReadyWithoutRemoteProvider;
                };
                if !build
                    .runtime_config
                    .remote_account_provider
                    .lifecycle_mode()
                    .needs_remote_account_provider()
                {
                    ChainlinkPrimaryReadiness::ReadyWithoutRemoteProvider
                } else {
                    ChainlinkPrimaryReadiness::NotReady
                }
            }
        }
    }

    /// Marks the non-primary lifecycle boundary. Non-primary chainlink calls
    /// intentionally become no-op/local-only while bank reset remains an
    /// explicit primary-readiness operation.
    pub async fn disable(&self) -> ChainlinkResult<()> {
        self.lifecycle_state
            .store(ChainlinkLifecycleState::Stopping as u8, Ordering::SeqCst);

        let runtime = self.runtime.lock().await.take();
        let Some(mut runtime) = runtime else {
            self.lifecycle_state.store(
                ChainlinkLifecycleState::Disabled as u8,
                Ordering::SeqCst,
            );
            info!("Chainlink remote sync already disabled by lifecycle transition");
            return Ok(());
        };

        if let Some(mut removed_accounts_sub) =
            runtime.removed_accounts_sub.take()
        {
            removed_accounts_sub.abort();
            if tokio::time::timeout(
                std::time::Duration::from_secs(2),
                &mut removed_accounts_sub,
            )
            .await
            .is_err()
            {
                warn!("Timed out waiting for Chainlink removed-account subscription to stop");
            }
        }

        let result = runtime.fetch_cloner.shutdown().await;
        self.lifecycle_state
            .store(ChainlinkLifecycleState::Disabled as u8, Ordering::SeqCst);
        if result.is_ok() {
            info!("Chainlink remote sync disabled by lifecycle transition");
        }
        result
    }

    #[cfg(any(test, feature = "dev-context"))]
    pub fn lifecycle_state_for_tests(&self) -> &'static str {
        match ChainlinkLifecycleState::from_u8(
            self.lifecycle_state.load(Ordering::SeqCst),
        ) {
            ChainlinkLifecycleState::Disabled => "disabled",
            ChainlinkLifecycleState::Starting => "starting",
            ChainlinkLifecycleState::Enabled => "enabled",
            ChainlinkLifecycleState::Stopping => "stopping",
        }
    }

    #[cfg(any(test, feature = "dev-context"))]
    pub async fn is_runtime_active(&self) -> bool {
        self.runtime.lock().await.is_some()
    }

    #[cfg(any(test, feature = "dev-context"))]
    pub async fn active_fetch_count_for_tests(&self) -> Option<u64> {
        self.runtime
            .lock()
            .await
            .as_ref()
            .map(|runtime| runtime.fetch_cloner.fetch_count())
    }

    pub fn try_new(
        accounts_bank: &Arc<V>,
        fetch_cloner: Option<Arc<FetchCloner<T, U, V, C>>>,
        validator_pubkey: Pubkey,
        config: &ChainLinkConfig,
    ) -> ChainlinkResult<Self> {
        let runtime = if let Some(fetch_cloner) = fetch_cloner {
            Some(Self::build_runtime(accounts_bank, fetch_cloner)?)
        } else {
            None
        };
        let lifecycle_state = if runtime.is_some() {
            ChainlinkLifecycleState::Enabled
        } else {
            ChainlinkLifecycleState::Disabled
        };
        Ok(Self {
            accounts_bank: accounts_bank.clone(),
            runtime: AsyncMutex::new(runtime),
            lifecycle_state: AtomicU8::new(lifecycle_state as u8),
            runtime_build_config: None,
            validator_id: validator_pubkey,
            remove_confined_accounts: config.remove_confined_accounts,
        })
    }

    fn build_runtime(
        accounts_bank: &Arc<V>,
        fetch_cloner: Arc<FetchCloner<T, U, V, C>>,
    ) -> ChainlinkResult<ChainlinkRuntime<T, U, V, C>> {
        let removed_accounts_rx = fetch_cloner.try_get_removed_account_rx()?;
        let cloner = fetch_cloner.cloner();
        let removed_accounts_sub = Some(Self::subscribe_account_removals(
            accounts_bank,
            cloner,
            removed_accounts_rx,
        ));
        Ok(ChainlinkRuntime {
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
    ) -> ChainlinkResult<
        Chainlink<ChainRpcClientImpl, SubMuxClient<ChainUpdatesClient>, V, C>,
    > {
        let validator_pubkey = validator_keypair.pubkey();
        let validator_keypair_bytes = validator_keypair.to_bytes();

        Ok(Chainlink {
            accounts_bank: accounts_bank.clone(),
            runtime: AsyncMutex::new(None),
            lifecycle_state: AtomicU8::new(
                ChainlinkLifecycleState::Disabled as u8,
            ),
            runtime_build_config: Some(ChainlinkRuntimeBuildConfig {
                endpoints: endpoints.clone(),
                commitment,
                cloner: cloner.clone(),
                validator_keypair_bytes,
                chainlink_config: chainlink_config.clone(),
                runtime_config: config,
                ledger_path: ledger_path.to_path_buf(),
            }),
            validator_id: validator_pubkey,
            remove_confined_accounts: chainlink_config.remove_confined_accounts,
        })
    }

    #[allow(dead_code)]
    fn validator_keypair_from_bytes(
        bytes: &[u8; 64],
    ) -> ChainlinkResult<Keypair> {
        Keypair::try_from(&bytes[..]).map_err(|err| {
            ChainlinkError::InvalidValidatorKeypair(err.to_string())
        })
    }

    /// Removes all accounts that aren't delegated to us and not blacklisted from the bank.
    /// This is explicit primary-readiness cleanup: standalone validators run it
    /// before entering Primary mode, and replicated validators run it during
    /// promotion. It is intentionally not controlled by the generic remote sync
    /// gates used by ensure/fetch calls.
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
        let Some(fetch_cloner) = self.runtime_fetch_cloner().await else {
            return Ok(FetchAndCloneResult::default());
        };
        self.fetch_accounts_common(
            fetch_cloner.as_ref(),
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
        let Some(fetch_cloner) = self.runtime_fetch_cloner().await else {
            // If we're offline and not syncing accounts then we just get them from the bank
            return Ok(pubkeys
                .iter()
                .map(|pubkey| self.accounts_bank.get_account(pubkey))
                .collect());
        };
        let _ = self
            .fetch_accounts_common(
                fetch_cloner.as_ref(),
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

        let Some(fetch_cloner) = self.runtime_fetch_cloner().await else {
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

    async fn runtime_fetch_cloner(
        &self,
    ) -> Option<Arc<FetchCloner<T, U, V, C>>> {
        if ChainlinkLifecycleState::from_u8(
            self.lifecycle_state.load(Ordering::SeqCst),
        ) != ChainlinkLifecycleState::Enabled
        {
            return None;
        }

        let runtime = self.runtime.lock().await;
        if ChainlinkLifecycleState::from_u8(
            self.lifecycle_state.load(Ordering::SeqCst),
        ) != ChainlinkLifecycleState::Enabled
        {
            return None;
        }
        runtime.as_ref().map(|runtime| runtime.fetch_cloner.clone())
    }

    #[cfg(any(test, feature = "dev-context"))]
    pub async fn fetch_cloner_for_tests(
        &self,
    ) -> Option<Arc<FetchCloner<T, U, V, C>>> {
        self.runtime_fetch_cloner().await
    }

    pub fn fetch_count(&self) -> Option<u64> {
        self.runtime.try_lock().ok().and_then(|runtime| {
            runtime
                .as_ref()
                .map(|runtime| runtime.fetch_cloner.fetch_count())
        })
    }

    pub fn is_watching(&self, pubkey: &Pubkey) -> bool {
        self.runtime
            .try_lock()
            .ok()
            .and_then(|runtime| {
                runtime
                    .as_ref()
                    .map(|runtime| runtime.fetch_cloner.is_watching(pubkey))
            })
            .unwrap_or(false)
    }
}

impl<V: AccountsBank, C: Cloner>
    Chainlink<ChainRpcClientImpl, SubMuxClient<ChainUpdatesClient>, V, C>
{
    /// Marks the primary lifecycle boundary for chainlink remote sync and
    /// creates the runtime from stored production configuration when needed.
    pub async fn enable_primary(
        &self,
    ) -> ChainlinkResult<PrimaryEnableOutcome> {
        let mut runtime = self.runtime.lock().await;
        if runtime.is_some() {
            self.lifecycle_state.store(
                ChainlinkLifecycleState::Enabled as u8,
                Ordering::SeqCst,
            );
            Self::log_primary_runtime_active();
            return Ok(PrimaryEnableOutcome::RuntimeActive);
        }

        self.lifecycle_state
            .store(ChainlinkLifecycleState::Starting as u8, Ordering::SeqCst);

        let Some(build) = self.runtime_build_config.as_ref() else {
            self.lifecycle_state.store(
                ChainlinkLifecycleState::Disabled as u8,
                Ordering::SeqCst,
            );
            Self::log_primary_disabled_by_config();
            return Ok(PrimaryEnableOutcome::DisabledByConfig);
        };

        if !build
            .runtime_config
            .remote_account_provider
            .lifecycle_mode()
            .needs_remote_account_provider()
        {
            self.lifecycle_state.store(
                ChainlinkLifecycleState::Disabled as u8,
                Ordering::SeqCst,
            );
            Self::log_primary_disabled_by_config();
            return Ok(PrimaryEnableOutcome::DisabledByConfig);
        }

        match self.create_runtime_from_build_config(build).await {
            Ok(new_runtime) => {
                *runtime = Some(new_runtime);
                self.lifecycle_state.store(
                    ChainlinkLifecycleState::Enabled as u8,
                    Ordering::SeqCst,
                );
                Self::log_primary_runtime_active();
                Ok(PrimaryEnableOutcome::RuntimeActive)
            }
            Err(err) => {
                *runtime = None;
                self.lifecycle_state.store(
                    ChainlinkLifecycleState::Disabled as u8,
                    Ordering::SeqCst,
                );
                Err(err)
            }
        }
    }

    async fn create_runtime_from_build_config(
        &self,
        build: &ChainlinkRuntimeBuildConfig<C>,
    ) -> ChainlinkResult<
        ChainlinkRuntime<
            ChainRpcClientImpl,
            SubMuxClient<ChainUpdatesClient>,
            V,
            C,
        >,
    > {
        let validator_keypair =
            Self::validator_keypair_from_bytes(&build.validator_keypair_bytes)?;
        let (tx, rx) = tokio::sync::mpsc::channel(5_000);
        let account_provider = RemoteAccountProvider::try_from_urls_and_config(
            &build.endpoints,
            build.commitment,
            tx,
            &build.runtime_config.remote_account_provider,
        )
        .await?;

        let Some(provider) = account_provider else {
            return Err(ChainlinkError::MissingRemoteAccountProvider);
        };

        let provider = Arc::new(provider);
        let risk_service = RiskService::try_from_config(
            &build.chainlink_config.risk,
            &build.ledger_path,
        )?
        .map(Arc::new);
        let fetch_cloner = FetchCloner::new(
            &provider,
            &self.accounts_bank,
            &build.cloner,
            validator_keypair,
            rx,
            build.chainlink_config.allowed_programs.clone(),
            risk_service,
        );
        Self::build_runtime(&self.accounts_bank, fetch_cloner)
    }

    fn log_primary_runtime_active() {
        info!("Chainlink primary remote sync enabled");
    }

    fn log_primary_disabled_by_config() {
        info!("Chainlink primary remote sync disabled by config");
    }
}

#[async_trait::async_trait]
impl<V, C> PrimaryChainlink
    for Chainlink<ChainRpcClientImpl, SubMuxClient<ChainUpdatesClient>, V, C>
where
    V: AccountsBank + Send + Sync + 'static,
    C: Cloner + Send + Sync + 'static,
{
    fn reset_accounts_bank(&self) -> AccountsDbResult<()> {
        Chainlink::reset_accounts_bank(self)
    }

    async fn enable_primary(&self) -> ChainlinkResult<PrimaryEnableOutcome> {
        Chainlink::enable_primary(self).await
    }

    async fn disable(&self) -> ChainlinkResult<()> {
        Chainlink::disable(self).await
    }

    async fn primary_runtime_readiness(&self) -> PrimaryRuntimeReadiness {
        Chainlink::primary_runtime_readiness(self).await
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
mod readiness_tests {
    use std::sync::Arc;

    use magicblock_accounts_db::AccountsDb;
    use magicblock_config::config::ChainLinkConfig;
    use solana_pubkey::Pubkey;

    use crate::{
        remote_account_provider::chain_pubsub_client::mock::ChainPubsubClientMock,
        testing::{
            cloner_stub::ClonerStub, rpc_client_mock::ChainRpcClientMock,
        },
    };

    use super::{Chainlink, ChainlinkPrimaryReadiness};

    #[tokio::test]
    async fn try_new_without_fetch_cloner_is_ready_without_remote_provider(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let tempdir = tempfile::tempdir()?;
        let accounts_db = Arc::new(AccountsDb::open(tempdir.path())?);
        let chainlink = Chainlink::<
            ChainRpcClientMock,
            ChainPubsubClientMock,
            AccountsDb,
            ClonerStub,
        >::try_new(
            &accounts_db,
            None,
            Pubkey::new_unique(),
            &ChainLinkConfig::default(),
        )?;

        assert_eq!(
            chainlink.primary_readiness().await,
            ChainlinkPrimaryReadiness::ReadyWithoutRemoteProvider
        );
        Ok(())
    }

    #[test]
    fn label_matches_expected_metric_values() {
        assert_eq!(ChainlinkPrimaryReadiness::Ready.label(), "ready");
        assert_eq!(
            ChainlinkPrimaryReadiness::ReadyWithoutRemoteProvider.label(),
            "ready_without_remote_provider"
        );
        assert_eq!(ChainlinkPrimaryReadiness::NotReady.label(), "not_ready");
    }

    #[test]
    fn allows_aperture_ensure_only_for_ready_variants() {
        assert!(ChainlinkPrimaryReadiness::Ready.allows_aperture_ensure());
        assert!(ChainlinkPrimaryReadiness::ReadyWithoutRemoteProvider
            .allows_aperture_ensure());
        assert!(!ChainlinkPrimaryReadiness::NotReady.allows_aperture_ensure());
    }
}
