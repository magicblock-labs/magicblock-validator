use std::{
    collections::{HashMap, HashSet},
    future::Future,
    mem,
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use dlp_api::{
    pda::delegation_record_pda_from_delegated_account,
    state::{
        DelegationRecord, UndelegationRequest,
        discriminator::AccountDiscriminator,
    },
};
use engine::Engine;
use keeper::MissingAccount;
use lru::LruCache;
use magicblock_aml::RiskService;
use magicblock_config::config::AllowedProgram;
use magicblock_core::token_programs::{
    ASSOCIATED_TOKEN_PROGRAM_ID, EATA_PROGRAM_ID, TOKEN_PROGRAM_ID, is_ata,
    normalize_native_token_account_for_local_clone,
};
use magicblock_metrics::metrics::{
    self, AccountFetchContext, AccountFetchReason, BankPrecheckOutcome,
    BankPrecheckReason, ChainlinkCloneIntent, ChainlinkCloneOutcome,
    ChainlinkCloneRemoteResult, ChainlinkCompanionFetchKind,
    ChainlinkEmptyPlaceholderStage, Outcome,
};
use parking_lot::Mutex as PlMutex;
use solana_account::{
    Account, AccountBuilder, AccountMode, AccountSharedData, ReadableAccount,
    StateFlags,
};
use solana_account_decoder_client_types::{
    UiAccountEncoding, UiDataSliceConfig,
};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{
    config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    filter::{Memcmp, RpcFilterType},
};
use solana_signer::Signer;
use tokio::{
    sync::{Semaphore, mpsc, watch},
    task,
    task::JoinSet,
};
use tracing::*;

fn immutable_account_mode(lamports: u64) -> AccountMode {
    if lamports == 0 {
        AccountMode::Placeholder
    } else {
        AccountMode::ReadOnly
    }
}

mod ata_projection;
mod delegation;
mod fetching;
mod ownership;
mod pending_ops;
mod pipeline;
mod program_loader;
mod subscription;
mod subscription_updates;
#[cfg(test)]
mod tests;
mod types;

pub use self::types::FetchAndCloneResult;
use self::{
    subscription::{SubscriptionRelease, release_subs},
    types::{
        AccountWithCompanion, ClassifiedAccounts, FetchAndCloneBatchResult,
        MaterializedAccount, PartitionedNotFound, RefreshDecision,
        ResolvedDelegatedAccounts, ResolvedPrograms,
    },
};
use super::errors::{ChainlinkError, ChainlinkResult};
use crate::{
    chainlink::{
        ObservedUndelegationRequest,
        account_still_undelegating_on_chain::account_still_undelegating_on_chain,
        record_mirror::{
            DelegationRecordMirror, DiscoveredDelegation, MirrorLookup,
        },
    },
    cloner::{
        self, AccountCloneRequest, AccountMaterialization,
        ClonePostDelegationMode, DelegationActions, errors::ClonerResult,
    },
    remote_account_provider::{
        ChainPubsubClient, ChainRpcClient, ForwardedSubscriptionUpdate,
        MatchSlotsConfig, RemoteAccount, RemoteAccountProvider,
        ResolvedAccount, SubscriptionReason,
        program_account::{
            LOADER_V3, LoadedProgram, RemoteProgramLoader,
            get_loaderv3_get_program_data_address,
        },
        pubsub_common::{
            SubscriptionSource, is_delegation_record_data,
            is_internal_dlp_account_data,
        },
    },
};

pub struct FetchCloner<T, U>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
{
    /// The RemoteAccountProvider to fetch accounts from
    remote_account_provider: Arc<RemoteAccountProvider<T, U>>,
    /// Counter to track the number of fetch operations for testing deduplication
    fetch_count: Arc<AtomicU64>,
    /// Counter to let tests await complete subscription-update processing.
    processed_updates_count: Arc<AtomicU64>,

    engine: Engine,
    validator_pubkey: Pubkey,
    validator_keypair: Arc<Keypair>,

    /// If specified, only these programs will be cloned. If None or empty,
    /// all programs are allowed.
    allowed_programs: Option<HashSet<Pubkey>>,

    /// Negative cache for derived eATAs confirmed missing on chain.
    known_empty_eatas: Arc<PlMutex<LruCache<Pubkey, ()>>>,

    /// Per-program state from the last successful load, so the per-slot
    /// notifications providers emit for heavily-invoked program accounts
    /// resolve without re-pulling full program data. Entries for programs
    /// evicted from the bank are pruned on their next notification.
    program_verify_cache: Arc<PlMutex<LruCache<Pubkey, ProgramVerifyState>>>,

    /// Maps watched LoaderV3 programdata accounts back to their program IDs.
    /// Programdata notifications are the reliable upgrade signal because the
    /// program account itself can remain byte-identical across upgrades.
    programdata_index: Arc<PlMutex<LruCache<Pubkey, Pubkey>>>,

    /// Internal-looking DLP updates awaiting a delayed mirror recheck.
    pending_collision_rechecks: Arc<PlMutex<LruCache<Pubkey, (Pubkey, u64)>>>,
    /// Bounds direct RPC reconciliation when the delayed set is full.
    collision_overflow_reconciliations: Arc<Semaphore>,

    pending_undelegations: Arc<Mutex<HashSet<Pubkey>>>,

    /// Risk checker for post-delegation action addresses.
    risk_service: Option<Arc<RiskService>>,

    record_mirror: Option<Arc<DelegationRecordMirror>>,

    undelegation_request_sender: mpsc::Sender<ObservedUndelegationRequest>,
}

struct PendingUndelegationGuard {
    pending_undelegations: Arc<Mutex<HashSet<Pubkey>>>,
    pubkey: Pubkey,
}

const MIRRORED_RECORD_LAMPORTS: u64 = 1_000_000;

impl Drop for PendingUndelegationGuard {
    fn drop(&mut self) {
        if let Ok(mut pending_undelegations) = self.pending_undelegations.lock()
        {
            pending_undelegations.remove(&self.pubkey);
        }
    }
}

/// Negative-cache capacity for known-empty eATAs.
const KNOWN_EMPTY_EATAS_CAPACITY: NonZeroUsize =
    match NonZeroUsize::new(100_000) {
        Some(n) => n,
        None => panic!("KNOWN_EMPTY_EATAS_CAPACITY must be non-zero"),
    };

/// Capacity of the program verify cache; far above the number of programs
/// a validator realistically loads, while bounding it across eviction churn.
const PROGRAM_VERIFY_CACHE_CAPACITY: NonZeroUsize = match NonZeroUsize::new(64)
{
    Some(n) => n,
    None => panic!("PROGRAM_VERIFY_CACHE_CAPACITY must be non-zero"),
};

/// Persistent programdata watches are bounded independently from the Engine
/// account cache because subscription ownership remains in Chainlink.
const PROGRAMDATA_WATCH_CAPACITY: NonZeroUsize =
    NonZeroUsize::new(512).expect("programdata watch capacity is non-zero");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgramDataWatch {
    Installed,
    AlreadyInstalled,
    EvictedConcurrently,
}

const PENDING_COLLISION_RECHECKS_CAPACITY: NonZeroUsize =
    NonZeroUsize::new(16_384).expect("collision recheck capacity is non-zero");
const COLLISION_OVERFLOW_RECONCILIATION_LIMIT: usize = 64;
const AUTHORITY_RECORD_SWEEP_INTERVAL: Duration = Duration::from_secs(300);
const REPLAY_RECOVERY_RETRY_DELAY: Duration = Duration::from_secs(30);
const COLLISION_RECHECK_DELAYS: [Duration; 3] = [
    Duration::from_secs(2),
    Duration::from_secs(8),
    Duration::from_secs(30),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DlpProgramUpdateInterest {
    DropLocalDelegatedAuthoritative,
    ProcessUndelegating,
    ProcessAtaProjection,
    ProcessDirectlyWatched,
    DiscoverDelegatedAccount,
}

fn authority_record_config(
    authority: Pubkey,
    min_context_slot: Option<u64>,
) -> RpcProgramAccountsConfig {
    RpcProgramAccountsConfig {
        filters: Some(vec![
            RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
                0,
                AccountDiscriminator::DelegationRecord.to_bytes().to_vec(),
            )),
            RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
                8,
                authority.to_bytes().to_vec(),
            )),
        ]),
        account_config: RpcAccountInfoConfig {
            encoding: Some(UiAccountEncoding::Base64Zstd),
            data_slice: Some(UiDataSliceConfig {
                offset: 0,
                length: 0,
            }),
            min_context_slot,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn replay_recovery_candidates(
    account_pubkeys: impl IntoIterator<Item = Pubkey>,
    record_pdas: &HashSet<Pubkey>,
) -> Vec<Pubkey> {
    account_pubkeys
        .into_iter()
        .filter(|pubkey| {
            record_pdas
                .contains(&delegation_record_pda_from_delegated_account(pubkey))
        })
        .collect()
}

impl<T, U> Clone for FetchCloner<T, U>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
{
    fn clone(&self) -> Self {
        Self {
            remote_account_provider: self.remote_account_provider.clone(),
            fetch_count: self.fetch_count.clone(),
            processed_updates_count: self.processed_updates_count.clone(),
            engine: self.engine.clone(),
            validator_pubkey: self.validator_pubkey,
            validator_keypair: Arc::clone(&self.validator_keypair),
            allowed_programs: self.allowed_programs.clone(),
            known_empty_eatas: self.known_empty_eatas.clone(),
            program_verify_cache: self.program_verify_cache.clone(),
            programdata_index: self.programdata_index.clone(),
            pending_collision_rechecks: self.pending_collision_rechecks.clone(),
            collision_overflow_reconciliations: self
                .collision_overflow_reconciliations
                .clone(),
            pending_undelegations: self.pending_undelegations.clone(),
            risk_service: self.risk_service.clone(),
            record_mirror: self.record_mirror.clone(),
            undelegation_request_sender: self
                .undelegation_request_sender
                .clone(),
        }
    }
}

/// State from the last successful program load driving the cheap
/// executable-update resolution in [program_loader].
#[derive(Debug, Clone)]
pub(crate) struct ProgramVerifyState {
    /// Raw programdata metadata prefix (tag + deploy slot + authority)
    /// from the last load; `None` for loaders without programdata.
    pub(crate) programdata_header: Option<Vec<u8>>,
    /// When the program was last loaded or verified against remote state.
    pub(crate) verified_at: std::time::Instant,
    /// A verification is scheduled for when the throttle window expires,
    /// covering notifications suppressed within the window.
    pub(crate) deferred_verify: bool,
}

#[derive(Debug, Clone)]
struct CompanionFetchLogContext {
    origin: AccountFetchContext,
    primary_pubkey: Pubkey,
    context_slot: u64,
}

fn log_companion_fetch_failure<E: std::fmt::Display + ?Sized>(
    ctx: &CompanionFetchLogContext,
    companion_pubkey: Pubkey,
    companion_kind: ChainlinkCompanionFetchKind,
    error: &E,
) {
    error!(
        primary_pubkey = %ctx.primary_pubkey,
        companion_pubkey = %companion_pubkey,
        companion_kind = %companion_kind,
        origin_entrypoint = %ctx.origin.entrypoint(),
        origin_reason = %ctx.origin.reason(),
        context_slot = ctx.context_slot,
        error = %error,
        "Failed to fetch companion account"
    );
}

impl<T, U> FetchCloner<T, U>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
{
    /// Create FetchCloner with subscription updates properly connected
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        remote_account_provider: &Arc<RemoteAccountProvider<T, U>>,
        engine: Engine,
        validator_keypair: Keypair,
        subscription_updates_rx: mpsc::Receiver<ForwardedSubscriptionUpdate>,
        allowed_programs: Option<Vec<AllowedProgram>>,
        risk_service: Option<Arc<RiskService>>,
    ) -> Arc<Self> {
        let (undelegation_request_sender, _) = mpsc::channel(1024);
        Self::new_with_undelegation_request_sender(
            remote_account_provider,
            engine,
            validator_keypair,
            subscription_updates_rx,
            allowed_programs,
            risk_service,
            None,
            undelegation_request_sender,
        )
    }

    /// Create FetchCloner with subscription updates and request notifications connected.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_undelegation_request_sender(
        remote_account_provider: &Arc<RemoteAccountProvider<T, U>>,
        engine: Engine,
        validator_keypair: Keypair,
        subscription_updates_rx: mpsc::Receiver<ForwardedSubscriptionUpdate>,
        allowed_programs: Option<Vec<AllowedProgram>>,
        risk_service: Option<Arc<RiskService>>,
        record_mirror: Option<Arc<DelegationRecordMirror>>,
        undelegation_request_sender: mpsc::Sender<ObservedUndelegationRequest>,
    ) -> Arc<Self> {
        let validator_pubkey = validator_keypair.pubkey();
        let allowed_programs = allowed_programs.map(|programs| {
            programs.iter().map(|p| p.id).collect::<HashSet<_>>()
        });
        let me = Arc::new(Self {
            remote_account_provider: remote_account_provider.clone(),
            engine,
            validator_pubkey,
            validator_keypair: Arc::new(validator_keypair),
            fetch_count: Arc::new(AtomicU64::new(0)),
            processed_updates_count: Arc::new(AtomicU64::new(0)),
            allowed_programs,
            known_empty_eatas: Arc::new(PlMutex::new(LruCache::new(
                KNOWN_EMPTY_EATAS_CAPACITY,
            ))),
            program_verify_cache: Arc::new(PlMutex::new(LruCache::new(
                PROGRAM_VERIFY_CACHE_CAPACITY,
            ))),
            programdata_index: Arc::new(PlMutex::new(LruCache::new(
                PROGRAMDATA_WATCH_CAPACITY,
            ))),
            pending_collision_rechecks: Arc::new(PlMutex::new(LruCache::new(
                PENDING_COLLISION_RECHECKS_CAPACITY,
            ))),
            collision_overflow_reconciliations: Arc::new(Semaphore::new(
                COLLISION_OVERFLOW_RECONCILIATION_LIMIT,
            )),
            pending_undelegations: Arc::new(Mutex::new(HashSet::new())),
            risk_service,
            record_mirror,
            undelegation_request_sender,
        });

        me.clone()
            .start_subscription_listener(subscription_updates_rx);

        if let Some(discoveries) = me
            .record_mirror
            .as_ref()
            .and_then(|mirror| mirror.take_discoveries())
        {
            me.clone().start_discovery_listener(discoveries);
            me.clone().start_authority_record_sweep();
        }
        if let Some(recovery_slots) = me
            .record_mirror
            .as_ref()
            .and_then(|mirror| mirror.take_replay_recovery_slots())
        {
            me.clone().start_replay_recovery_listener(recovery_slots);
        }

        me
    }

    fn start_replay_recovery_listener(
        self: Arc<Self>,
        mut recovery_slots: watch::Receiver<u64>,
    ) {
        let weak = Arc::downgrade(&self);
        drop(self);
        task::spawn(async move {
            while recovery_slots.changed().await.is_ok() {
                let mut recovery_slot = *recovery_slots.borrow_and_update();
                loop {
                    let Some(this) = weak.upgrade() else { return };
                    match this.reconcile_replay_gap(recovery_slot).await {
                        Ok(count) => {
                            info!(
                                recovery_slot,
                                count, "record replay gap reconciled"
                            );
                            break;
                        }
                        Err(error) => warn!(
                            ?error,
                            recovery_slot,
                            "record replay gap recovery failed; retrying"
                        ),
                    }
                    drop(this);

                    tokio::select! {
                        changed = recovery_slots.changed() => {
                            if changed.is_err() {
                                return;
                            }
                            recovery_slot = *recovery_slots.borrow_and_update();
                        }
                        _ = tokio::time::sleep(REPLAY_RECOVERY_RETRY_DELAY) => {}
                    }
                }
            }
        });
    }

    async fn reconcile_replay_gap(
        &self,
        min_context_slot: u64,
    ) -> ChainlinkResult<usize> {
        let delegation_program = dlp_api::id();
        let local_records = self
            .remote_account_provider
            .get_program_accounts_with_config(
                &delegation_program,
                authority_record_config(
                    self.validator_pubkey,
                    Some(min_context_slot),
                ),
            );
        let confined_records = self
            .remote_account_provider
            .get_program_accounts_with_config(
                &delegation_program,
                authority_record_config(
                    Pubkey::default(),
                    Some(min_context_slot),
                ),
            );
        let (local_records, confined_records) =
            tokio::try_join!(local_records, confined_records)?;
        let record_pdas = local_records
            .into_iter()
            .chain(confined_records)
            .map(|(pubkey, _)| pubkey)
            .collect::<HashSet<_>>();

        let accounts = self
            .remote_account_provider
            .get_program_accounts_with_config(
                &delegation_program,
                RpcProgramAccountsConfig {
                    account_config: RpcAccountInfoConfig {
                        encoding: Some(UiAccountEncoding::Base64Zstd),
                        data_slice: Some(UiDataSliceConfig {
                            offset: 0,
                            length: 0,
                        }),
                        min_context_slot: Some(min_context_slot),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await?;
        let candidates = replay_recovery_candidates(
            accounts.into_iter().map(|(pubkey, _)| pubkey),
            &record_pdas,
        );
        for pubkey in candidates.iter().copied() {
            self.resolve_internal_dlp_collision(pubkey, min_context_slot)
                .await;
        }
        Ok(candidates.len())
    }

    fn start_authority_record_sweep(self: Arc<Self>) {
        let weak = Arc::downgrade(&self);
        drop(self);
        task::spawn(async move {
            loop {
                tokio::time::sleep(AUTHORITY_RECORD_SWEEP_INTERVAL).await;
                let Some(this) = weak.upgrade() else { return };
                let config =
                    authority_record_config(this.validator_pubkey, None);
                match this
                    .remote_account_provider
                    .get_program_accounts_with_config(&dlp_api::id(), config)
                    .await
                {
                    Ok(records) => {
                        let count = records
                            .iter()
                            .filter(|(pubkey, _)| {
                                !this.contains_account(pubkey)
                            })
                            .count();
                        metrics::set_authority_records_on_chain(count as i64);
                    }
                    Err(error) => warn!(
                        ?error,
                        "authority record sweep failed; retrying next interval"
                    ),
                }
            }
        });
    }

    fn start_discovery_listener(
        self: Arc<Self>,
        mut discoveries: mpsc::Receiver<DiscoveredDelegation>,
    ) {
        let weak = Arc::downgrade(&self);
        drop(self);
        task::spawn(async move {
            while let Some(discovered) = discoveries.recv().await {
                let Some(this) = weak.upgrade() else { return };
                this.resolve_internal_dlp_collision(
                    discovered.delegated_account,
                    discovered.slot,
                )
                .await;
            }
        });
    }

    fn invalidate_mirrored_record(&self, pubkey: &Pubkey) {
        if let Some(mirror) = &self.record_mirror {
            mirror.invalidate(&delegation_record_pda_from_delegated_account(
                pubkey,
            ));
        }
    }

    /// Get the current fetch count
    pub fn fetch_count(&self) -> u64 {
        self.fetch_count.load(Ordering::Relaxed)
    }

    async fn classify_dlp_program_update_interest(
        &self,
        pubkey: Pubkey,
        account: &AccountSharedData,
    ) -> DlpProgramUpdateInterest {
        if let Some((undelegating, delegated)) =
            self.read_account(&pubkey, |account| {
                (
                    account.is(AccountMode::Transient),
                    account.is(AccountMode::Delegated),
                )
            })
        {
            if undelegating {
                return DlpProgramUpdateInterest::ProcessUndelegating;
            }
            if delegated {
                return DlpProgramUpdateInterest::DropLocalDelegatedAuthoritative;
            }
        }

        if self.remote_account_provider.is_watching(&pubkey) {
            return DlpProgramUpdateInterest::ProcessDirectlyWatched;
        }

        if let Some(ata_pubkeys) =
            ata_projection::derive_supported_ata_pubkeys_from_raw_eata(
                &pubkey,
                account.data(),
            )
        {
            let has_projection_interest = self
                .raw_eata_has_local_projection_interest(&pubkey, &ata_pubkeys)
                .await;
            return if has_projection_interest {
                DlpProgramUpdateInterest::ProcessAtaProjection
            } else {
                DlpProgramUpdateInterest::DiscoverDelegatedAccount
            };
        }

        if self
            .base_ata_has_projection_interest(pubkey, account.data())
            .await
        {
            return DlpProgramUpdateInterest::ProcessAtaProjection;
        }

        DlpProgramUpdateInterest::DiscoverDelegatedAccount
    }

    async fn raw_eata_has_local_projection_interest(
        &self,
        pubkey: &Pubkey,
        ata_pubkeys: &[Pubkey],
    ) -> bool {
        if ata_pubkeys
            .iter()
            .any(|ata_pubkey| self.contains_account(ata_pubkey))
        {
            return true;
        }

        self.remote_account_provider
            .has_any_subscription_reason(
                ata_pubkeys.iter().chain(std::iter::once(pubkey)),
                SubscriptionReason::AtaProjection,
            )
            .await
    }

    async fn base_ata_has_projection_interest(
        &self,
        pubkey: Pubkey,
        data: &[u8],
    ) -> bool {
        let Some(eata_pubkey) =
            ata_projection::derive_eata_pubkey_from_ata_layout(&pubkey, data)
        else {
            return false;
        };

        self.remote_account_provider
            .has_subscription_reason(&pubkey, SubscriptionReason::AtaProjection)
            .await
            || self
                .remote_account_provider
                .has_subscription_reason(
                    &eata_pubkey,
                    SubscriptionReason::AtaProjection,
                )
                .await
    }

    /// Number of subscription updates whose processing has finished.
    pub fn processed_updates_count(&self) -> u64 {
        self.processed_updates_count.load(Ordering::Acquire)
    }

    #[instrument(skip(self, pubkeys))]
    pub async fn fetch_remote_accounts(
        &self,
        pubkeys: &[Pubkey],
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<Vec<RemoteAccount>> {
        Ok(self
            .remote_account_provider
            .try_get_multi(pubkeys, None, fetch_context, None)
            .await?)
    }

    pub async fn fetch_undelegation_requests(
        &self,
    ) -> ChainlinkResult<Vec<ObservedUndelegationRequest>> {
        let observed_slot = self.remote_account_provider.get_slot().await?;
        let config = RpcProgramAccountsConfig {
            filters: Some(vec![
                RpcFilterType::DataSize(
                    UndelegationRequest::size_with_discriminator() as u64,
                ),
                RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
                    0,
                    AccountDiscriminator::UndelegationRequest
                        .to_bytes()
                        .to_vec(),
                )),
            ]),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64Zstd),
                ..Default::default()
            },
            ..Default::default()
        };
        let accounts = self
            .remote_account_provider
            .get_program_accounts_with_config(&dlp_api::id(), config)
            .await?;

        let mut requests = Vec::with_capacity(accounts.len());
        for (request_pda, account) in accounts {
            let Ok(request) =
                UndelegationRequest::try_from_bytes_with_discriminator(
                    &account.data,
                )
            else {
                warn!(
                    request_pda = %request_pda,
                    data_len = account.data.len(),
                    "Skipping malformed DLP undelegation request account"
                );
                continue;
            };
            requests.push(ObservedUndelegationRequest {
                request_pda,
                delegated_account: request.delegated_account,
                expires_at_slot: request.expires_at_slot,
                observed_slot,
            });
        }

        Ok(requests)
    }

    pub(crate) fn engine(&self) -> &Engine {
        &self.engine
    }

    pub(crate) fn read_account<R>(
        &self,
        pubkey: &Pubkey,
        reader: impl Fn(&AccountSharedData) -> R,
    ) -> Option<R> {
        self.engine
            .accounts()
            .loader()
            .read(pubkey, reader)
            .ok()
            .flatten()
    }

    pub(crate) fn contains_account(&self, pubkey: &Pubkey) -> bool {
        self.engine
            .accounts()
            .loader()
            .contains(pubkey)
            .unwrap_or(false)
    }

    pub(crate) fn remote_account_provider(
        &self,
    ) -> &Arc<RemoteAccountProvider<T, U>> {
        &self.remote_account_provider
    }

    /// Check if a program is allowed to be cloned.
    /// Returns true if:
    /// - No allowed_programs restriction is set (None), OR
    /// - The allowed_programs set is empty (treats empty as unrestricted), OR
    /// - The program is in the allowed_programs set
    fn is_program_allowed(&self, program_id: &Pubkey) -> bool {
        match &self.allowed_programs {
            None => true,
            Some(allowed) => {
                if allowed.is_empty() {
                    true
                } else {
                    allowed.contains(program_id)
                }
            }
        }
    }

    fn program_subscription_is_too_broad(&self, program_id: &Pubkey) -> bool {
        program_id == &TOKEN_PROGRAM_ID
            || program_id == &spl_token_2022::id()
            || program_id == &ASSOCIATED_TOKEN_PROGRAM_ID
    }

    fn is_empty_placeholder_account(account: &AccountBuilder) -> bool {
        account.read().is(AccountMode::Placeholder)
    }

    fn clone_remote_result_for_request(
        request: &AccountCloneRequest,
    ) -> ChainlinkCloneRemoteResult {
        if Self::is_empty_placeholder_account(&request.account) {
            ChainlinkCloneRemoteResult::NotFound
        } else {
            ChainlinkCloneRemoteResult::Found
        }
    }

    fn clone_intent_for_request(
        request: &AccountCloneRequest,
    ) -> ChainlinkCloneIntent {
        if Self::is_empty_placeholder_account(&request.account) {
            ChainlinkCloneIntent::EmptyPlaceholder
        } else if request.account.read().is(AccountMode::Delegated) {
            ChainlinkCloneIntent::DelegationRecord
        } else if request.post_delegation_mode.has_actions() {
            ChainlinkCloneIntent::ActionDependency
        } else {
            ChainlinkCloneIntent::NormalAccount
        }
    }

    fn record_empty_placeholder_stage(
        is_empty_placeholder: bool,
        fetch_context: AccountFetchContext,
        stage: ChainlinkEmptyPlaceholderStage,
        outcome: Outcome,
    ) {
        if is_empty_placeholder {
            metrics::inc_chainlink_empty_placeholder_accounts_total_with_context(
                fetch_context,
                stage,
                outcome,
            );
        }
    }

    /// Check if an account is currently watched by the remote provider.
    pub fn is_watching(&self, pubkey: &Pubkey) -> bool {
        self.remote_account_provider.is_watching(pubkey)
    }

    /// Subscribe to updates for a specific account
    /// This is typically used when an account is about to be undelegated
    /// and we need to start watching for changes
    #[instrument(skip(self))]
    pub(crate) async fn acquire_subscription_reason(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
    ) -> ChainlinkResult<()> {
        self.remote_account_provider
            .acquire_subscription(pubkey, reason)
            .await
            .map_err(|err| {
                ChainlinkError::FailedToSubscribeToAccount(*pubkey, err)
            })
    }

    pub(crate) async fn ensure_subscription(
        &self,
        pubkey: &Pubkey,
        reason: SubscriptionReason,
    ) -> ChainlinkResult<()> {
        self.remote_account_provider
            .ensure_subscription(pubkey, reason)
            .await
            .map_err(|err| {
                ChainlinkError::FailedToSubscribeToAccount(*pubkey, err)
            })
    }

    #[instrument(skip(self))]
    pub async fn subscribe_to_account_to_track_undelegation(
        &self,
        pubkey: &Pubkey,
    ) -> ChainlinkResult<()> {
        trace!(
            pubkey = %pubkey,
            reason = ?SubscriptionReason::UndelegationTracking,
            "Subscribing to account"
        );
        // This ownership outlives the direct subscription while the account is
        // transitioning back to readonly.
        self.acquire_subscription_reason(
            pubkey,
            SubscriptionReason::UndelegationTracking,
        )
        .await
    }

    pub fn chain_slot(&self) -> u64 {
        self.remote_account_provider.chain_slot()
    }

    pub fn received_updates_count(&self) -> u64 {
        self.remote_account_provider.received_updates_count()
    }

    pub fn try_get_stale_account_rx(
        &self,
    ) -> ChainlinkResult<mpsc::Receiver<Pubkey>> {
        Ok(self.remote_account_provider.try_get_stale_account_rx()?)
    }
}
