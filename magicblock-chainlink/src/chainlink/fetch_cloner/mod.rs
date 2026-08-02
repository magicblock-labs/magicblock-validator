use std::{
    collections::{hash_map, HashSet},
    num::NonZeroUsize,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use dlp_api::{
    pda::delegation_record_pda_from_delegated_account,
    state::{discriminator::AccountDiscriminator, UndelegationRequest},
};
use lru::LruCache;
use magicblock_accounts_db::traits::AccountsBank;
use magicblock_aml::RiskService;
use magicblock_config::config::AllowedProgram;
use magicblock_metrics::metrics::{
    self, AccountFetchContext, ChainlinkCompanionFetchKind,
};
use parking_lot::Mutex as PlMutex;
use scc::HashMap;
use solana_account::AccountSharedData;
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
    sync::{broadcast, mpsc, oneshot},
    task,
};
use tracing::*;

pub(crate) const FETCH_CLONE_OPERATION_TIMEOUT: Duration =
    Duration::from_secs(60);

mod ata_projection;
mod delegation;
mod fetching;
mod ownership;
mod pending_clone_guard;
mod pending_operation;
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
    pending_clone_guard::{CloneClaim, CloneCompletion, PendingCloneGuard},
    pending_operation::{
        claim_or_join_pending, finish_pending, Pending, PendingClaim,
        PendingFailure, PendingHandles, PendingOwner, PendingTerminal,
        PendingWaiter,
    },
    subscription::{release_subs, SubscriptionRelease},
    types::{
        AccountWithCompanion, ClassifiedAccounts, PartitionedNotFound,
        RefreshDecision, ResolvedDelegatedAccounts, ResolvedPrograms,
    },
};
use super::errors::{ChainlinkError, ChainlinkResult};
use crate::{
    chainlink::{
        account_still_undelegating_on_chain::account_still_undelegating_on_chain,
        blacklisted_accounts::{
            blacklisted_accounts, programs_not_to_subscribe,
        },
        record_mirror::{
            DelegationRecordMirror, DiscoveredDelegation, MirrorLookup,
        },
        ObservedUndelegationRequest,
    },
    cloner::{
        errors::{ClonerError, ClonerResult},
        AccountCloneRequest, Cloner, DelegationActions,
    },
    remote_account_provider::{
        program_account::{
            get_loaderv3_get_program_data_address, LoadedProgram,
        },
        pubsub_common::{
            is_delegation_record_data, is_internal_dlp_account_data,
            SubscriptionSource,
        },
        CapacityEvictionProtection, ChainPubsubClient, ChainRpcClient,
        ForwardedSubscriptionUpdate, MatchSlotsConfig, RemoteAccount,
        RemoteAccountProvider, ResolvedAccountSharedData, SubscriptionReason,
    },
};

pub struct FetchCloner<T, U, V, C>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    /// The RemoteAccountProvider to fetch accounts from
    remote_account_provider: Arc<RemoteAccountProvider<T, U>>,
    /// Tracks pending account fetch requests to avoid duplicate fetches in parallel
    /// Once an account is fetched and cloned into the bank, it's removed from here
    pending_requests: Arc<HashMap<Pubkey, Pending>>,
    /// Monotonic generation for pending request ownership. Guards must match
    /// the stored generation before they can complete or clean up an entry.
    pending_request_generation: Arc<AtomicU64>,
    pending_waiter_generation: Arc<AtomicU64>,
    /// Counter to track the number of fetch operations for testing deduplication
    fetch_count: Arc<AtomicU64>,

    accounts_bank: Arc<V>,
    cloner: Arc<C>,
    validator_pubkey: Pubkey,
    validator_keypair: Arc<Keypair>,

    /// These are accounts that we should never clone into our validator.
    /// native programs, sysvars, native tokens, validator identity and faucet
    blacklisted_accounts: HashSet<Pubkey>,

    /// If specified, only these programs will be cloned. If None or empty,
    /// all programs are allowed.
    allowed_programs: Option<HashSet<Pubkey>>,

    /// Programs too broad for `subscribe_program`.
    programs_not_to_subscribe: HashSet<Pubkey>,

    /// Negative cache for derived eATAs confirmed missing on chain.
    known_empty_eatas: Arc<PlMutex<LruCache<Pubkey, ()>>>,

    /// Per-program state from the last successful load, so the per-slot
    /// notifications providers emit for heavily-invoked program accounts
    /// resolve without re-pulling full program data. Entries for programs
    /// evicted from the bank are pruned on their next notification.
    program_verify_cache: Arc<PlMutex<LruCache<Pubkey, ProgramVerifyState>>>,

    /// Recognizes freshly delegated accounts whose app data collides with an
    /// internal DLP discriminator via delegation-record sightings.
    /// Internal-looking DLP updates awaiting one delayed mirror recheck,
    /// keyed by derived delegation-record PDA -> (pubkey, max update slot).
    pending_collision_rechecks: Arc<PlMutex<LruCache<Pubkey, (Pubkey, u64)>>>,

    /// Tracks in-flight clone operations.
    /// The first caller to claim a key becomes the owner and performs
    /// the actual clone. Subsequent callers become waiters and receive
    /// the result via oneshot channels. Prevents duplicate clone
    /// submissions across concurrent fetch and subscription paths.
    pending_clones: Arc<
        Mutex<hash_map::HashMap<Pubkey, Vec<oneshot::Sender<CloneCompletion>>>>,
    >,

    pending_undelegations: Arc<Mutex<HashSet<Pubkey>>>,

    pending_operation_timeout_ms: Arc<AtomicU64>,

    /// Risk checker for post-delegation action addresses.
    risk_service: Option<Arc<RiskService>>,

    /// In-memory delegation-record mirror consulted before RPC record
    /// fetches; any miss falls back to the RPC path.
    record_mirror: Option<Arc<DelegationRecordMirror>>,

    undelegation_request_sender: broadcast::Sender<ObservedUndelegationRequest>,
}

struct PendingUndelegationGuard {
    pending_undelegations: Arc<Mutex<HashSet<Pubkey>>>,
    pubkey: Pubkey,
}

impl Drop for PendingUndelegationGuard {
    fn drop(&mut self) {
        if let Ok(mut pending_undelegations) = self.pending_undelegations.lock()
        {
            pending_undelegations.remove(&self.pubkey);
        }
    }
}

/// Lamports for a record companion synthesized from mirrored bytes.
/// Downstream consumers only read the companion's data, never its lamports.
const MIRRORED_RECORD_LAMPORTS: u64 = 1_000_000;

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

/// Capacity for internal-looking account updates awaiting one delayed
/// mirror recheck; bounds memory across record-heavy firehose bursts.
const PENDING_COLLISION_RECHECKS_CAPACITY: NonZeroUsize =
    match NonZeroUsize::new(16_384) {
        Some(n) => n,
        None => {
            panic!("PENDING_COLLISION_RECHECKS_CAPACITY must be non-zero")
        }
    };

/// Interval between authority-record sweeps: an observational gPA counting
/// on-chain records delegated to this validator, used to detect drift
/// against locally delegated state once the DLP firehose is gone.
const AUTHORITY_RECORD_SWEEP_INTERVAL: Duration = Duration::from_secs(300);

/// Backoff schedule for re-consulting the record mirror on a collision
/// candidate: the account update and its delegation record arrive on
/// different streams, so the record may lag the account — briefly in
/// steady state, longer across a mirror reconnect that replays the stream.
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

/// A pending fetch+clone operation claimed by one dedup call, resolved by
/// the batch worker spawned for that call.
struct ClaimedOperation {
    pubkey: Pubkey,
    generation: u64,
    deadline: tokio::time::Instant,
    cancel: Arc<tokio::sync::Notify>,
    owner: PendingOwner,
}

/// Manual Clone impl: `#[derive(Clone)]` would add `V: Clone, C: Clone`
/// bounds that are not satisfied (`AccountsBank` and `Cloner` don't
/// require `Clone`). All fields are behind `Arc` so Clone is not needed.
impl<T, U, V, C> Clone for FetchCloner<T, U, V, C>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    fn clone(&self) -> Self {
        Self {
            remote_account_provider: self.remote_account_provider.clone(),
            pending_requests: self.pending_requests.clone(),
            pending_request_generation: self.pending_request_generation.clone(),
            pending_waiter_generation: self.pending_waiter_generation.clone(),
            fetch_count: self.fetch_count.clone(),
            accounts_bank: self.accounts_bank.clone(),
            cloner: self.cloner.clone(),
            validator_pubkey: self.validator_pubkey,
            validator_keypair: Arc::clone(&self.validator_keypair),
            blacklisted_accounts: self.blacklisted_accounts.clone(),
            allowed_programs: self.allowed_programs.clone(),
            programs_not_to_subscribe: self.programs_not_to_subscribe.clone(),
            known_empty_eatas: self.known_empty_eatas.clone(),
            program_verify_cache: self.program_verify_cache.clone(),
            pending_collision_rechecks: self.pending_collision_rechecks.clone(),
            pending_clones: self.pending_clones.clone(),
            pending_undelegations: self.pending_undelegations.clone(),
            pending_operation_timeout_ms: self
                .pending_operation_timeout_ms
                .clone(),
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

impl<T, U, V, C> FetchCloner<T, U, V, C>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    /// Create FetchCloner with subscription updates properly connected
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        remote_account_provider: &Arc<RemoteAccountProvider<T, U>>,
        accounts_bank: &Arc<V>,
        cloner: &Arc<C>,
        validator_keypair: Keypair,
        subscription_updates_rx: mpsc::Receiver<ForwardedSubscriptionUpdate>,
        allowed_programs: Option<Vec<AllowedProgram>>,
        risk_service: Option<Arc<RiskService>>,
    ) -> Arc<Self> {
        let (undelegation_request_sender, _) = broadcast::channel(1024);
        Self::new_with_undelegation_request_sender(
            remote_account_provider,
            accounts_bank,
            cloner,
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
        accounts_bank: &Arc<V>,
        cloner: &Arc<C>,
        validator_keypair: Keypair,
        subscription_updates_rx: mpsc::Receiver<ForwardedSubscriptionUpdate>,
        allowed_programs: Option<Vec<AllowedProgram>>,
        risk_service: Option<Arc<RiskService>>,
        record_mirror: Option<Arc<DelegationRecordMirror>>,
        undelegation_request_sender: broadcast::Sender<
            ObservedUndelegationRequest,
        >,
    ) -> Arc<Self> {
        let validator_pubkey = validator_keypair.pubkey();
        let blacklisted_accounts = blacklisted_accounts(&validator_pubkey);
        let allowed_programs = allowed_programs.map(|programs| {
            programs.iter().map(|p| p.id).collect::<HashSet<_>>()
        });
        let me = Arc::new(Self {
            remote_account_provider: remote_account_provider.clone(),
            accounts_bank: accounts_bank.clone(),
            cloner: cloner.clone(),
            validator_pubkey,
            validator_keypair: Arc::new(validator_keypair),
            pending_requests: Arc::new(HashMap::new()),
            pending_request_generation: Arc::new(AtomicU64::new(1)),
            pending_waiter_generation: Arc::new(AtomicU64::new(1)),
            fetch_count: Arc::new(AtomicU64::new(0)),
            blacklisted_accounts,
            allowed_programs,
            programs_not_to_subscribe: programs_not_to_subscribe(),
            known_empty_eatas: Arc::new(PlMutex::new(LruCache::new(
                KNOWN_EMPTY_EATAS_CAPACITY,
            ))),
            program_verify_cache: Arc::new(PlMutex::new(LruCache::new(
                PROGRAM_VERIFY_CACHE_CAPACITY,
            ))),
            pending_collision_rechecks: Arc::new(PlMutex::new(LruCache::new(
                PENDING_COLLISION_RECHECKS_CAPACITY,
            ))),
            pending_clones: Arc::new(Mutex::new(hash_map::HashMap::new())),
            pending_undelegations: Arc::new(Mutex::new(HashSet::new())),
            pending_operation_timeout_ms: Arc::new(AtomicU64::new(
                FETCH_CLONE_OPERATION_TIMEOUT.as_millis() as u64,
            )),
            risk_service,
            record_mirror,
            undelegation_request_sender,
        });

        let accounts_bank_for_eviction = accounts_bank.clone();
        me.remote_account_provider.set_capacity_eviction_protection(
            move |pubkey| {
                accounts_bank_for_eviction
                    .get_account(pubkey)
                    .map(|account| CapacityEvictionProtection {
                        delegated: account.delegated(),
                        undelegating: account.undelegating(),
                    })
                    .unwrap_or(CapacityEvictionProtection {
                        delegated: false,
                        undelegating: false,
                    })
            },
        );
        me.clone()
            .start_subscription_listener(subscription_updates_rx);

        // Discovery from the record stream: delegations observed on chain
        // (naming the delegated account) resolve through the same probe/
        // recheck path as colliding firehose updates — mirror-proven
        // authority, forced-refresh clone, no per-event RPC.
        if let Some(discoveries) = me
            .record_mirror
            .as_ref()
            .and_then(|mirror| mirror.take_discoveries())
        {
            me.clone().start_discovery_listener(discoveries);
            me.clone().start_authority_record_sweep();
        }

        me
    }

    /// Periodically counts on-chain delegation records naming this
    /// validator as authority. Purely observational: with the DLP account
    /// firehose gone, this gauge (compared against locally delegated
    /// accounts) is the drift detector for missed record-stream events.
    ///
    /// Holds only a weak reference so the task ends with the FetchCloner.
    fn start_authority_record_sweep(self: Arc<Self>) {
        let weak = Arc::downgrade(&self);
        drop(self);
        task::spawn(async move {
            loop {
                tokio::time::sleep(AUTHORITY_RECORD_SWEEP_INTERVAL).await;
                let Some(this) = weak.upgrade() else { return };
                let config = RpcProgramAccountsConfig {
                    filters: Some(vec![
                        RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
                            0,
                            AccountDiscriminator::DelegationRecord
                                .to_bytes()
                                .to_vec(),
                        )),
                        // DelegationRecord.authority sits right after the
                        // 8-byte discriminator.
                        RpcFilterType::Memcmp(Memcmp::new_raw_bytes(
                            8,
                            this.validator_pubkey.to_bytes().to_vec(),
                        )),
                    ]),
                    account_config: RpcAccountInfoConfig {
                        encoding: Some(UiAccountEncoding::Base64Zstd),
                        data_slice: Some(UiDataSliceConfig {
                            offset: 0,
                            length: 0,
                        }),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                match this
                    .remote_account_provider
                    .get_program_accounts_with_config(&dlp_api::id(), config)
                    .await
                {
                    Ok(records) => {
                        // A delegated application account can mimic the
                        // record layout (discriminator + authority bytes);
                        // genuine records are never bank-resident, so
                        // in-bank matches are collisions, not records.
                        let count = records
                            .iter()
                            .filter(|(pubkey, _)| {
                                this.accounts_bank.get_account(pubkey).is_none()
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

    /// Holds only a weak reference so the task ends with the FetchCloner.
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

    /// Get the current fetch count
    pub fn fetch_count(&self) -> u64 {
        self.fetch_count.load(Ordering::Relaxed)
    }

    /// Drops the mirror entry for an account's delegation record when local
    /// state observed its undelegation completing — insurance against a
    /// missed tombstone leaving a stale delegated entry behind.
    fn invalidate_mirrored_record(&self, pubkey: &Pubkey) {
        if let Some(mirror) = &self.record_mirror {
            mirror.invalidate(&delegation_record_pda_from_delegated_account(
                pubkey,
            ));
        }
    }

    async fn classify_dlp_program_update_interest(
        &self,
        pubkey: Pubkey,
        account: &AccountSharedData,
    ) -> DlpProgramUpdateInterest {
        if let Some(local_account) = self.accounts_bank.get_account(&pubkey) {
            if local_account.undelegating() {
                return DlpProgramUpdateInterest::ProcessUndelegating;
            }
            if local_account.delegated() {
                return DlpProgramUpdateInterest::DropLocalDelegatedAuthoritative;
            }
        }

        if self.remote_account_provider.is_watching(&pubkey) {
            return DlpProgramUpdateInterest::ProcessDirectlyWatched;
        }

        if let Some(ata_pubkeys) =
            ata_projection::derive_supported_ata_pubkeys_from_raw_eata(
                &pubkey, account,
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

        if self.base_ata_has_projection_interest(pubkey, account).await {
            return DlpProgramUpdateInterest::ProcessAtaProjection;
        }

        DlpProgramUpdateInterest::DiscoverDelegatedAccount
    }

    async fn raw_eata_has_local_projection_interest(
        &self,
        pubkey: &Pubkey,
        ata_pubkeys: &[Pubkey],
    ) -> bool {
        if ata_pubkeys.iter().any(|ata_pubkey| {
            self.accounts_bank.get_account(ata_pubkey).is_some()
        }) {
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
        account: &AccountSharedData,
    ) -> bool {
        let Some(eata_pubkey) =
            ata_projection::derive_eata_pubkey_from_ata_account(
                &pubkey, account,
            )
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

    pub fn cloner(&self) -> &Arc<C> {
        &self.cloner
    }

    pub(crate) fn remote_account_provider(
        &self,
    ) -> &Arc<RemoteAccountProvider<T, U>> {
        &self.remote_account_provider
    }
}
