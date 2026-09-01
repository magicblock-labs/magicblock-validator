use std::{
    collections::HashSet,
    future::Future,
    mem,
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc,
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
use keeper::error::KeeperError;
use lru::LruCache;
use magicblock_aml::RiskService;
use magicblock_config::config::{AllowedProgram, AmlCheckStrategy};
use magicblock_core::token_programs::{
    ASSOCIATED_TOKEN_PROGRAM_ID, EATA_PROGRAM_ID, TOKEN_2022_PROGRAM_ID,
    TOKEN_PROGRAM_ID, is_ata, normalize_native_token_account_for_local_clone,
};
use magicblock_metrics::metrics::{
    self, AccountFetchContext, AccountFetchReason, BankPrecheckOutcome,
    BankPrecheckReason, ChainlinkCloneIntent, ChainlinkCloneOutcome,
    ChainlinkCloneRemoteResult, ChainlinkCompanionFetchKind,
    ChainlinkEmptyPlaceholderStage, Outcome,
};
use parking_lot::Mutex as PlMutex;
use solana_account::{
    AccountBuilder, AccountMode, AccountSharedData, ReadableAccount, StateFlags,
};
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{
    config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    filter::{Memcmp, RpcFilterType},
};
use solana_signer::Signer;
use tokio::{
    sync::{Semaphore, broadcast, mpsc},
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
mod pipeline;
mod program_loader;
mod subscription;
#[cfg(test)]
mod tests;
mod types;

pub use self::types::FetchAndCloneResult;
use self::{
    subscription::{SubscriptionRelease, release_subs},
    types::{
        AccountWithCompanion, ClassifiedAccounts, FetchAndCloneBatchResult,
        PartitionedNotFound, RefreshDecision, ResolvedDelegatedAccounts,
        ResolvedPrograms,
    },
};
use super::errors::{ChainlinkError, ChainlinkResult};
use crate::{
    chainlink::{
        ObservedUndelegationRequest,
        account_still_undelegating_on_chain::account_still_undelegating_on_chain,
    },
    cloner::{
        self, AccountCloneRequest, ClonePostDelegationMode, DelegationActions,
        errors::ClonerResult,
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

    /// Rotates the starting entry of each programdata sweep pass so an
    /// abandoned pass cannot starve the same tail entries every time.
    programdata_sweep_cursor: Arc<AtomicU64>,

    /// Recognizes freshly delegated accounts whose app data collides with an
    /// internal DLP discriminator via delegation-record sightings.
    dlp_collision_tracker: Arc<PlMutex<DlpCollisionTracker>>,

    /// Risk checker for post-delegation action addresses.
    risk_service: Option<Arc<RiskService>>,

    undelegation_request_sender: broadcast::Sender<ObservedUndelegationRequest>,
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

/// Interval between transport-independent sweeps over the programdata
/// watches. A watch whose upstream subscription silently dies stops
/// delivering upgrade notifications entirely; the sweep bounds upgrade
/// detection for such programs to roughly this interval, at a cost of one
/// 12-byte header fetch per watched program.
const PROGRAMDATA_SWEEP_INTERVAL: Duration = Duration::from_secs(300);

/// Programdata prefix holding the state tag (4 bytes) and deploy slot
/// (8 bytes) of `UpgradeableLoaderState::ProgramData`.
const PROGRAMDATA_DEPLOY_SLOT_PREFIX_LEN: usize = 12;

/// Enum tag of `UpgradeableLoaderState::ProgramData`.
const PROGRAMDATA_STATE_TAG: u32 = 3;

/// Consecutive header-fetch failures after which a sweep pass is
/// abandoned until the next interval, bounding a pass against an
/// unhealthy RPC endpoint.
const PROGRAMDATA_SWEEP_MAX_CONSECUTIVE_FAILURES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgramDataWatch {
    Installed,
    AlreadyInstalled,
    EvictedConcurrently,
}

/// Capacity for recently sighted delegation-record update slots; sized to
/// outlast DLP firehose churn across the SubMux debounce window.
const SEEN_DELEGATION_RECORD_SLOTS_CAPACITY: NonZeroUsize =
    NonZeroUsize::new(65_536).expect("collision tracker capacity is non-zero");
const PARKED_COLLISION_UPDATES_CAPACITY: NonZeroUsize =
    NonZeroUsize::new(16_384).expect("collision tracker capacity is non-zero");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DlpProgramUpdateInterest {
    DropLocalDelegatedAuthoritative,
    ProcessUndelegating,
    ProcessAtaProjection,
    ProcessDirectlyWatched,
    DiscoverDelegatedAccount,
}

/// A parked internal-looking account update, reduced to pubkey + slot so
/// the firehose cannot pin account payloads in memory.
#[derive(Debug, Clone, Copy)]
struct ParkedCollisionCandidate {
    pubkey: Pubkey,
    slot: u64,
}

struct DlpCollisionTracker {
    record_slots: LruCache<Pubkey, u64>,
    parked: LruCache<Pubkey, ParkedCollisionCandidate>,
}

impl DlpCollisionTracker {
    fn new() -> Self {
        Self {
            record_slots: LruCache::new(SEEN_DELEGATION_RECORD_SLOTS_CAPACITY),
            parked: LruCache::new(PARKED_COLLISION_UPDATES_CAPACITY),
        }
    }

    fn sight_record(
        &mut self,
        record_pubkey: Pubkey,
        slot: u64,
    ) -> Option<ParkedCollisionCandidate> {
        let sighted_slot =
            self.record_slots.get_or_insert_mut(record_pubkey, || slot);
        *sighted_slot = (*sighted_slot).max(slot);
        let sighted_slot = *sighted_slot;
        self.parked
            .peek(&record_pubkey)
            .is_some_and(|candidate| sighted_slot >= candidate.slot)
            .then(|| self.parked.pop(&record_pubkey))
            .flatten()
    }

    fn preserve_released_candidate(
        &mut self,
        candidate: ParkedCollisionCandidate,
    ) -> bool {
        let record_pubkey =
            delegation_record_pda_from_delegated_account(&candidate.pubkey);
        if !self.parked.contains(&record_pubkey)
            && self.parked.len() >= PARKED_COLLISION_UPDATES_CAPACITY.get()
        {
            return false;
        }

        let parked = self.parked.get_or_insert_mut(record_pubkey, || candidate);
        parked.slot = parked.slot.max(candidate.slot);
        true
    }

    fn check_or_park(&mut self, update: &ForwardedSubscriptionUpdate) -> bool {
        let record_pubkey =
            delegation_record_pda_from_delegated_account(&update.pubkey);
        let sighted = self
            .record_slots
            .get(&record_pubkey)
            .is_some_and(|&record_slot| record_slot >= update.account.slot());
        if !sighted {
            if !self.parked.contains(&record_pubkey)
                && self.parked.len() >= PARKED_COLLISION_UPDATES_CAPACITY.get()
            {
                return false;
            }

            let parked = self.parked.get_or_insert_mut(record_pubkey, || {
                ParkedCollisionCandidate {
                    pubkey: update.pubkey,
                    slot: update.account.slot(),
                }
            });
            parked.slot = parked.slot.max(update.account.slot());
        }
        sighted
    }
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
            programdata_sweep_cursor: self.programdata_sweep_cursor.clone(),
            dlp_collision_tracker: self.dlp_collision_tracker.clone(),
            risk_service: self.risk_service.clone(),
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

/// Programs whose presence in a post-delegation action triggers a risk check
/// under [`AmlCheckStrategy::RelevantPrograms`]: SPL Token (legacy and 2022),
/// the ephemeral SPL / eATA program (ESPL), the Magic program, and the system
/// programs. The latter move native SOL, which is as much value as a token
/// balance, so leaving them out would let a plain lamport transfer signed by a
/// sanctioned address through unchecked.
const RISK_RELEVANT_PROGRAMS: [Pubkey; 6] = [
    TOKEN_PROGRAM_ID,
    TOKEN_2022_PROGRAM_ID,
    EATA_PROGRAM_ID,
    magicblock_magic_program_api::ID,
    solana_sdk_ids::system_program::ID,
    magicblock_magic_program_api::EPHEMERAL_SYSTEM_PROGRAM_ID,
];

/// Decides whether the configured [`AmlCheckStrategy`] requires risk checking
/// the signers of these post-delegation actions.
fn delegation_actions_require_risk_check(
    strategy: AmlCheckStrategy,
    delegation_actions: &[solana_instruction::Instruction],
) -> bool {
    match strategy {
        AmlCheckStrategy::AllSigners => true,
        AmlCheckStrategy::RelevantPrograms => delegation_actions
            .iter()
            .any(instruction_involves_risk_relevant_program),
    }
}

/// Returns true when a risk-relevant program is invoked by the instruction or
/// referenced by any of its accounts (e.g. as the target of a CPI).
fn instruction_involves_risk_relevant_program(
    instruction: &solana_instruction::Instruction,
) -> bool {
    RISK_RELEVANT_PROGRAMS.contains(&instruction.program_id)
        || instruction
            .accounts
            .iter()
            .any(|meta| RISK_RELEVANT_PROGRAMS.contains(&meta.pubkey))
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
        let (undelegation_request_sender, _) = broadcast::channel(1024);
        Self::new_with_undelegation_request_sender(
            remote_account_provider,
            engine,
            validator_keypair,
            subscription_updates_rx,
            allowed_programs,
            risk_service,
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
        undelegation_request_sender: broadcast::Sender<
            ObservedUndelegationRequest,
        >,
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
            programdata_sweep_cursor: Arc::new(AtomicU64::new(0)),
            dlp_collision_tracker: Arc::new(PlMutex::new(
                DlpCollisionTracker::new(),
            )),
            risk_service,
            undelegation_request_sender,
        });

        me.clone()
            .start_subscription_listener(subscription_updates_rx);
        me.start_programdata_sweep();

        me
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

    async fn submit_account(
        &self,
        accessor: &mut engine::AccountAccessor<'_>,
        request: AccountCloneRequest,
        fetch_context: AccountFetchContext,
    ) -> ClonerResult<()> {
        let remote_result = Self::clone_remote_result_for_request(&request);
        let clone_intent = Self::clone_intent_for_request(&request);
        let is_empty_placeholder =
            Self::is_empty_placeholder_account(&request.account);
        metrics::inc_chainlink_clone_accounts_total_with_context(
            fetch_context.clone(),
            remote_result,
            clone_intent,
            ChainlinkCloneOutcome::Submitted,
        );
        Self::record_empty_placeholder_stage(
            is_empty_placeholder,
            fetch_context.clone(),
            ChainlinkEmptyPlaceholderStage::CloneSubmitted,
            Outcome::Success,
        );
        let result =
            cloner::clone_account(&self.engine, accessor, request).await;
        if result.is_ok() {
            metrics::inc_chainlink_clone_accounts_total_with_context(
                fetch_context.clone(),
                remote_result,
                clone_intent,
                ChainlinkCloneOutcome::CloneSucceeded,
            );
        } else {
            metrics::inc_chainlink_clone_accounts_total_with_context(
                fetch_context.clone(),
                remote_result,
                clone_intent,
                ChainlinkCloneOutcome::CloneFailed,
            );
            metrics::inc_chainlink_clone_accounts_total_with_context(
                fetch_context.clone(),
                remote_result,
                clone_intent,
                ChainlinkCloneOutcome::SubmitFailed,
            );
            Self::record_empty_placeholder_stage(
                is_empty_placeholder,
                fetch_context,
                ChainlinkEmptyPlaceholderStage::CloneSubmitFailed,
                Outcome::Error,
            );
        }
        result
    }

    async fn watch_programdata(
        &self,
        program_id: Pubkey,
    ) -> ChainlinkResult<ProgramDataWatch> {
        let program_data_pubkey =
            get_loaderv3_get_program_data_address(&program_id);
        let evicted = {
            let mut index = self.programdata_index.lock();
            if index.get(&program_data_pubkey).is_some() {
                return Ok(ProgramDataWatch::AlreadyInstalled);
            }
            index.push(program_data_pubkey, program_id)
        };
        if let Some((evicted_program_data, evicted_program_id)) = evicted {
            debug!(
                program_id = %evicted_program_id,
                program_data = %evicted_program_data,
                "Releasing least-recently loaded programdata watch at capacity"
            );
            self.remote_account_provider
                .forget_subscription_reason(
                    &evicted_program_data,
                    SubscriptionReason::ProgramData,
                )
                .await;
            if let Err(err) =
                cloner::evict_account(&self.engine, evicted_program_id).await
            {
                warn!(
                    program_id = %evicted_program_id,
                    error = %err,
                    "Failed to evict program whose upgrade watch was released"
                );
            }
        }
        if let Err(err) = self
            .acquire_subscription_reason(
                &program_data_pubkey,
                SubscriptionReason::ProgramData,
            )
            .await
        {
            error!(
                program_id = %program_id,
                program_data = %program_data_pubkey,
                error = %err,
                "Failed to hold programdata subscription; upgrades may go undetected"
            );
            self.programdata_index.lock().pop(&program_data_pubkey);
            return Err(err);
        }
        if self
            .programdata_index
            .lock()
            .get(&program_data_pubkey)
            .is_none()
        {
            self.remote_account_provider
                .forget_subscription_reason(
                    &program_data_pubkey,
                    SubscriptionReason::ProgramData,
                )
                .await;
            return Ok(ProgramDataWatch::EvictedConcurrently);
        }
        self.remote_account_provider
            .prefer_grpc_subscription(&program_id)
            .await;
        Ok(ProgramDataWatch::Installed)
    }

    async fn unwatch_programdata(&self, program_id: Pubkey) {
        let program_data_pubkey =
            get_loaderv3_get_program_data_address(&program_id);
        if self
            .programdata_index
            .lock()
            .pop(&program_data_pubkey)
            .is_some()
        {
            self.remote_account_provider
                .forget_subscription_reason(
                    &program_data_pubkey,
                    SubscriptionReason::ProgramData,
                )
                .await;
        }
    }

    /// Runs the upgrade-detection backstop on a fixed interval. Holds only
    /// a weak handle so the loop neither keeps the FetchCloner alive nor
    /// outlives it.
    fn start_programdata_sweep(self: &Arc<Self>) {
        let this = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(PROGRAMDATA_SWEEP_INTERVAL).await;
                let Some(this) = this.upgrade() else {
                    break;
                };
                this.sweep_programdata_watches().await;
            }
        });
    }

    /// One pass of the upgrade-detection backstop: compares each watched
    /// program's on-chain deploy slot against the bank copy's remote slot
    /// and routes stale programs into the reload path. Runs on the fetch
    /// transport, so upgrades surface even when a watch subscription died
    /// upstream while local bookkeeping kept it. Metadata-only changes
    /// (e.g. authority rotations) keep the deploy slot and are left to the
    /// subscription path.
    async fn sweep_programdata_watches(&self) {
        let watched: Vec<(Pubkey, Pubkey)> = self
            .programdata_index
            .lock()
            .iter()
            .map(|(program_data, program_id)| (*program_data, *program_id))
            .collect();
        if watched.is_empty() {
            return;
        }

        let start = self
            .programdata_sweep_cursor
            .fetch_add(1, Ordering::Relaxed) as usize
            % watched.len();
        let len = watched.len();
        let mut consecutive_failures = 0usize;
        for (program_data_pubkey, program_id) in
            watched.into_iter().cycle().skip(start).take(len)
        {
            let Some(bank_slot) =
                self.read_account(&program_id, |account| account.slot())
            else {
                continue;
            };
            let prefix = match self
                .remote_account_provider
                .get_account_data_slice(
                    &program_data_pubkey,
                    0,
                    PROGRAMDATA_DEPLOY_SLOT_PREFIX_LEN,
                    bank_slot,
                )
                .await
            {
                Ok(Some(prefix))
                    if prefix.len() >= PROGRAMDATA_DEPLOY_SLOT_PREFIX_LEN
                        && prefix[..4]
                            == PROGRAMDATA_STATE_TAG.to_le_bytes() =>
                {
                    consecutive_failures = 0;
                    prefix
                }
                Ok(_) => {
                    consecutive_failures = 0;
                    continue;
                }
                Err(err) => {
                    debug!(
                        program_id = %program_id,
                        program_data = %program_data_pubkey,
                        error = %err,
                        "Programdata sweep header fetch failed"
                    );
                    consecutive_failures += 1;
                    if consecutive_failures
                        >= PROGRAMDATA_SWEEP_MAX_CONSECUTIVE_FAILURES
                    {
                        break;
                    }
                    continue;
                }
            };
            let Ok(deploy_slot_bytes) = <[u8; 8]>::try_from(&prefix[4..12])
            else {
                continue;
            };
            let deploy_slot = u64::from_le_bytes(deploy_slot_bytes);
            if deploy_slot <= bank_slot {
                continue;
            }
            info!(
                program_id = %program_id,
                deploy_slot,
                bank_slot,
                "Programdata sweep detected missed program upgrade"
            );
            let companion_fetch_log_context = CompanionFetchLogContext {
                origin: AccountFetchContext::subscription_update(
                    AccountFetchReason::SubscriptionUpdateClone,
                ),
                primary_pubkey: program_id,
                context_slot: deploy_slot,
            };
            let program_account =
                AccountBuilder::from(AccountSharedData::new(1, 0, &LOADER_V3))
                    .slot(deploy_slot);
            self.handle_executable_sub_update(
                program_id,
                program_account,
                &companion_fetch_log_context,
            )
            .await;
        }
    }

    async fn clone_program(
        &self,
        program: LoadedProgram,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<()> {
        let program_id = program.program_id;
        let is_loaderv3 = matches!(program.loader, RemoteProgramLoader::V3);
        let remote_result = ChainlinkCloneRemoteResult::Found;
        let clone_intent = ChainlinkCloneIntent::ProgramData;
        let Some(mut request) = cloner::resolve_program(program) else {
            metrics::inc_chainlink_clone_accounts_total_with_context(
                fetch_context,
                remote_result,
                clone_intent,
                ChainlinkCloneOutcome::Skipped,
            );
            return Ok(());
        };
        let installed_watch = is_loaderv3
            && matches!(
                self.watch_programdata(program_id).await,
                Ok(ProgramDataWatch::Installed)
            );

        let Some(mut accessor) =
            cloner::claim_materialization(&self.engine, &mut request).await?
        else {
            metrics::inc_chainlink_clone_accounts_total_with_context(
                fetch_context,
                remote_result,
                clone_intent,
                ChainlinkCloneOutcome::Skipped,
            );
            return Ok(());
        };

        metrics::inc_chainlink_clone_accounts_total_with_context(
            fetch_context.clone(),
            remote_result,
            clone_intent,
            ChainlinkCloneOutcome::Submitted,
        );
        let result = cloner::clone_program(&mut accessor, request)
            .await
            .map_err(ChainlinkError::from);
        drop(accessor);
        if result.is_ok() {
            if is_loaderv3 {
                let _ = self.watch_programdata(program_id).await;
            }
            metrics::inc_chainlink_clone_accounts_total_with_context(
                fetch_context.clone(),
                remote_result,
                clone_intent,
                ChainlinkCloneOutcome::CloneSucceeded,
            );
        } else {
            if installed_watch {
                self.unwatch_programdata(program_id).await;
            }
            metrics::inc_chainlink_clone_accounts_total_with_context(
                fetch_context.clone(),
                remote_result,
                clone_intent,
                ChainlinkCloneOutcome::CloneFailed,
            );
            metrics::inc_chainlink_clone_accounts_total_with_context(
                fetch_context,
                remote_result,
                clone_intent,
                ChainlinkCloneOutcome::SubmitFailed,
            );
        }
        result
    }

    async fn clone_account_with_post_delegation_action_invariants(
        &self,
        mut request: AccountCloneRequest,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<()> {
        if request.account.read().is(AccountMode::Delegated)
            && is_ata(
                &request.pubkey,
                request.account.read().owner(),
                request.account.read().data(),
            )
            .is_some()
        {
            request.account =
                normalize_native_token_account_for_local_clone(request.account)
                    .ok_or_else(|| {
                        ChainlinkError::InvalidTokenAccount(
                            request.pubkey,
                            "delegated ATA token data is malformed".to_string(),
                        )
                    })?;
        }
        self.normalize_unresolved_dlp_clone_request(&mut request)?;
        Self::normalize_immutable_account(&mut request);

        let ClonePostDelegationMode::ExecuteActions(delegation) =
            &request.post_delegation_mode
        else {
            let Some(mut accessor) =
                cloner::claim_materialization(&self.engine, &mut request)
                    .await?
            else {
                return Ok(());
            };
            return self
                .submit_account(&mut accessor, request, fetch_context)
                .await
                .map_err(Into::into);
        };

        if !request.account.read().is(AccountMode::Delegated) {
            return Err(ChainlinkError::InvalidDelegationActions(
                request.pubkey,
                "post-delegation actions attached to non-delegated clone target"
                    .to_string(),
            ));
        }
        let source_program = delegation.source_program();

        let dependency_error = self
            .ensure_delegation_action_dependencies(
                request.pubkey,
                request.account.read().slot(),
                delegation,
                fetch_context.clone(),
            )
            .await
            .err();

        let Some(mut accessor) =
            cloner::claim_materialization(&self.engine, &mut request).await?
        else {
            return Ok(());
        };
        if let Some(err) = dependency_error {
            request.post_delegation_mode =
                ClonePostDelegationMode::RescueUndelegate(source_program);
            return self
                .rescue_failed_activation(
                    &mut accessor,
                    request,
                    fetch_context,
                    err,
                )
                .await;
        }
        // Keep the shared account buffer for fallback without cloning the
        // action instructions that move into the activation attempt.
        let rescue = AccountCloneRequest {
            pubkey: request.pubkey,
            account: request.account.clone(),
            commit_frequency_ms: request.commit_frequency_ms,
            post_delegation_mode: ClonePostDelegationMode::RescueUndelegate(
                source_program,
            ),
            delegated_to_other: request.delegated_to_other,
        };
        match self
            .submit_account(&mut accessor, request, fetch_context.clone())
            .await
        {
            Ok(()) => Ok(()),
            Err(err) => {
                self.rescue_failed_activation(
                    &mut accessor,
                    rescue,
                    fetch_context,
                    err.into(),
                )
                .await
            }
        }
    }

    async fn rescue_failed_activation(
        &self,
        accessor: &mut engine::AccountAccessor<'_>,
        request: AccountCloneRequest,
        fetch_context: AccountFetchContext,
        err: ChainlinkError,
    ) -> ChainlinkResult<()> {
        let pubkey = request.pubkey;
        warn!(
            pubkey = %pubkey,
            error = ?err,
            "Post-delegation actions could not be satisfied; undelegating"
        );
        match self.submit_account(accessor, request, fetch_context).await {
            Ok(()) => Ok(()),
            Err(undelegation_err) => {
                warn!(
                    pubkey = %pubkey,
                    error = ?err,
                    undelegation_error = ?undelegation_err,
                    "Failed to schedule undelegation after post-delegation action clone failure"
                );
                Err(err)
            }
        }
    }

    fn normalize_unresolved_dlp_clone_request(
        &self,
        request: &mut AccountCloneRequest,
    ) -> ChainlinkResult<()> {
        // Both modes are claims that this validator owns the account: confined
        // accounts used to carry the delegated flag as well, so a single
        // `delegated()` check covered them. With exclusive modes they have to be
        // named separately, or a stale confinement would never be normalized.
        let claims_delegation =
            request.account.read().is(AccountMode::Delegated)
                || request.account.read().is(AccountMode::Ephemeral);
        if request.account.read().owner() != dlp_api::id() || !claims_delegation
        {
            return Ok(());
        }

        if request.pubkey
            == dlp_api::pda::magic_fee_vault_pda_from_validator(
                &self.validator_pubkey,
            )
        {
            return Ok(());
        }

        if request.post_delegation_mode.has_actions() {
            return Err(ChainlinkError::InvalidDelegationActions(
                request.pubkey,
                "post-delegation actions attached to unresolved DLP-owned clone target"
                    .to_string(),
            ));
        }

        request.account =
            mem::take(&mut request.account).mode(AccountMode::ReadOnly);
        Ok(())
    }

    fn normalize_immutable_account(request: &mut AccountCloneRequest) {
        let account = request.account.read();
        if !matches!(
            account.mode(),
            AccountMode::ReadOnly | AccountMode::Placeholder
        ) {
            return;
        }

        let mode = immutable_account_mode(account.lamports());
        if mode == account.mode() {
            return;
        }

        request.account = mem::take(&mut request.account).mode(mode);
    }

    pub fn start_subscription_listener(
        self: Arc<Self>,
        mut subscription_updates: mpsc::Receiver<ForwardedSubscriptionUpdate>,
    ) {
        tokio::spawn(async move {
            let semaphore =
                Arc::new(Semaphore::new(super::SUBSCRIPTION_UPDATE_LIMIT));
            let mut pending_tasks: JoinSet<()> = JoinSet::new();

            loop {
                while let Some(result) = pending_tasks.try_join_next() {
                    if let Err(err) = result {
                        warn!(error = ?err, "Subscription update task panicked");
                    }
                }

                // INVARIANT: The semaphore is created locally and never closed,
                // so acquire_owned() cannot fail with AcquireError.
                let permit = Arc::clone(&semaphore)
                    .acquire_owned()
                    .await
                    .expect("subscription update semaphore never closed");

                match subscription_updates.recv().await {
                    Some(update) => {
                        let pubkey = update.pubkey;
                        trace!(
                            pubkey = %pubkey,
                            "FetchCloner received subscription update"
                        );
                        let this = Arc::clone(&self);
                        metrics::inc_inflight_subscription_updates();
                        pending_tasks.spawn(async move {
                            struct InflightSubscriptionUpdateGuard;
                            impl Drop for InflightSubscriptionUpdateGuard {
                                fn drop(&mut self) {
                                    metrics::dec_inflight_subscription_updates(
                                    );
                                }
                            }
                            let _inflight_guard =
                                InflightSubscriptionUpdateGuard;

                            Self::process_subscription_update(
                                &this, pubkey, update,
                            )
                            .await;
                            this.processed_updates_count
                                .fetch_add(1, Ordering::Release);
                            drop(permit);
                        });
                    }
                    None => {
                        drop(permit);
                        while pending_tasks.join_next().await.is_some() {}
                        break;
                    }
                }
            }
        });
    }

    async fn process_subscription_update(
        &self,
        pubkey: Pubkey,
        update: ForwardedSubscriptionUpdate,
    ) {
        let fresh_update_account = update.account.fresh_account();
        let is_dlp_owned_update = fresh_update_account
            .is_some_and(|account| account.owner() == &dlp_api::id());
        let is_internal_dlp_update =
            fresh_update_account.is_some_and(|account| {
                is_internal_dlp_account_data(account.data())
            });

        let dlp_program_interest =
            if matches!(update.source, SubscriptionSource::Program)
                && is_dlp_owned_update
            {
                match fresh_update_account {
                    Some(account) => Some(
                        self.classify_dlp_program_update_interest(
                            pubkey, account,
                        )
                        .await,
                    ),
                    None => None,
                }
            } else {
                None
            };

        match dlp_program_interest {
            Some(DlpProgramUpdateInterest::DropLocalDelegatedAuthoritative) => {
                self.cleanup_direct_subscription_for_delegated_account(pubkey)
                    .await;
                trace!(
                    pubkey = %pubkey,
                    "Dropping DLP program update for locally authoritative delegated account"
                );
                return;
            }
            Some(DlpProgramUpdateInterest::ProcessUndelegating)
            | Some(DlpProgramUpdateInterest::ProcessAtaProjection)
            | Some(DlpProgramUpdateInterest::ProcessDirectlyWatched)
            | Some(DlpProgramUpdateInterest::DiscoverDelegatedAccount)
            | None => {}
        }

        // Internal DLP payloads (records/metadata/commit state) can never be
        // greedily cloned, so drop them before discovery issues remote
        // fetches. The exception is an account whose app data collides with
        // an internal discriminator: its delegation also writes the
        // delegation record, whose sighting routes the account update to
        // discovery — immediately when the record arrived first, or by
        // releasing the parked update once the record arrives later.
        if !matches!(
            dlp_program_interest,
            Some(DlpProgramUpdateInterest::ProcessUndelegating)
                | Some(DlpProgramUpdateInterest::ProcessAtaProjection)
        ) && is_dlp_owned_update
            && let Some(account) = fresh_update_account
            && is_internal_dlp_update
        {
            // Sight records from either source: SubMux dedup can
            // deliver a directly watched record account-sourced.
            let released = is_delegation_record_data(account.data())
                .then(|| {
                    self.dlp_collision_tracker
                        .lock()
                        .sight_record(pubkey, update.account.slot())
                })
                .flatten();
            if let Some(released) = released {
                self.clone_released_collision_candidate(released).await;
            }
            // Only the program firehose is dropped/parked.
            if matches!(update.source, SubscriptionSource::Program)
                && !self.dlp_collision_tracker.lock().check_or_park(&update)
            {
                trace!(
                    pubkey = %pubkey,
                    "Dropping internal DLP program subscription update"
                );
                return;
            }
        }

        if self
            .maybe_greedily_clone_discovered_delegated_account(pubkey, &update)
            .await
        {
            return;
        }
        // A late forwarded update can arrive after an account was removed from
        // the provider watch set. If a new subscription already won the race,
        // is_watching is true and this update can be processed normally. If this
        // update wins before acquire_subscription completes, the update is dropped;
        // the new subscription path performs its own fetch and clones fresh state.
        // If stale state is still present locally, cleanup is routed through the
        // existing removal listener, which serializes the final is_watching check and
        // eviction submission against same-pubkey subscription transitions.
        //
        // The guard only applies to account-subscription updates: the
        // account-sub pubsub tracking is the source of truth for `is_watching`. Program
        // subscription updates can legitimately arrive for pubkeys that are
        // *not* in the account-sub pubsub tracking (e.g. delegated accounts whose direct
        // subscription was released after cloning and are now tracked only via
        // their owner program). Dropping those would leave the bank stuck in a
        // stale delegated/undelegated state.
        let update_slot = update.account.slot();
        if matches!(update.source, SubscriptionSource::Account)
            && !self.remote_account_provider.is_watching(&pubkey)
        {
            trace!(
                pubkey = %pubkey,
                update_slot,
                "Dropping subscription update for account that is no longer watched"
            );
            if self.contains_account(&pubkey)
                && let Err(err) = self
                    .remote_account_provider
                    .send_stale_account(pubkey)
                    .await
            {
                warn!(
                    pubkey = %pubkey,
                    error = ?err,
                    "Failed to enqueue stale subscription update removal"
                );
            }
            return;
        }

        let companion_fetch_log_context = CompanionFetchLogContext {
            origin: AccountFetchContext::subscription_update(
                AccountFetchReason::SubscriptionUpdateClone,
            ),
            primary_pubkey: pubkey,
            context_slot: update_slot,
        };

        let routed_program_id =
            self.programdata_index.lock().get(&pubkey).copied();
        if let Some(program_id) = routed_program_id {
            let program_account =
                AccountBuilder::from(AccountSharedData::new(1, 0, &LOADER_V3))
                    .slot(update_slot);
            self.handle_executable_sub_update(
                program_id,
                program_account,
                &companion_fetch_log_context,
            )
            .await;
            return;
        }

        let update_source = update.source;
        let (resolved_account, deleg_record, delegation_actions) = self
            .resolve_account_to_clone_from_forwarded_sub_with_unsubscribe(
                update,
                &companion_fetch_log_context,
            )
            .await;
        let Some(account) = resolved_account else {
            return;
        };
        let subscription_clone_context =
            AccountFetchContext::subscription_update(
                AccountFetchReason::SubscriptionUpdateClone,
            );
        let projected_ata_clone_request = self
            .maybe_build_projected_ata_clone_request_from_subscription_update_with_source(
                pubkey,
                &account,
                update_source,
                deleg_record.as_ref(),
                delegation_actions.as_ref(),
                &companion_fetch_log_context,
            )
            .await;

        //
        // Ensure that the subscription update isn't out of order, i.e.
        // we already hold a newer version of the account in our bank.
        //
        // The stricter intent is to ignore non-advancing subscription updates: if the bank
        // already has the account at the same slot, then a normal/plain update at that slot is
        // treated as stale/duplicate and should not overwrite local state, with the following
        // exception:
        //
        //  - In the undelegate/redelegate same-slot path, the bank can still hold a plain
        //    or undelegating version while the subscription update carries the delegated state
        //    at the same slot, so we must allow that update.
        //
        let reader = |in_bank: &AccountSharedData| {
            let bank_slot = in_bank.slot();
            let update_slot = account.read().slot();
            let same_slot_delegated_refresh = bank_slot == update_slot
                && account.read().is(AccountMode::Delegated)
                && (!in_bank.is(AccountMode::Delegated)
                    || in_bank.is(AccountMode::Transient));
            if bank_slot > update_slot
                || (bank_slot == update_slot && !same_slot_delegated_refresh)
            {
                Some(bank_slot)
            } else {
                None
            }
        };
        let non_advancing_slot = self.read_account(&pubkey, reader).flatten();

        if let Some(in_bank_slot) = non_advancing_slot {
            let update_slot = account.read().slot();
            if in_bank_slot == update_slot
                && let Some(projected_ata_clone_request) =
                    projected_ata_clone_request
                && let Err(err) = self
                    .clone_projected_ata_request(
                        projected_ata_clone_request,
                        subscription_clone_context,
                    )
                    .await
            {
                warn!(
                    pubkey = %pubkey,
                    error = %err,
                    "Failed to clone projected ATA from out-of-order delegated eATA update"
                );
            }
            trace!(
                pubkey = %pubkey,
                bank_slot = in_bank_slot,
                update_slot,
                "Ignoring out-of-order subscription update"
            );
            return;
        }

        let mut undelegation_completed_on_chain = false;
        let reader = |in_bank: &AccountSharedData| {
            (
                in_bank.is(AccountMode::Delegated),
                in_bank.is(AccountMode::Transient),
                *in_bank.owner(),
                in_bank.slot(),
            )
        };
        if let Some((delegated, transient, owner, slot)) =
            self.read_account(&pubkey, reader)
        {
            if delegated && !transient {
                self.cleanup_direct_subscription_for_delegated_account(pubkey)
                    .await;
                return;
            }

            if transient {
                debug!(
                    pubkey = %pubkey,
                    in_bank_delegated = delegated,
                    in_bank_owner = %owner,
                    in_bank_slot = slot,
                    chain_delegated = account.read().is(AccountMode::Delegated),
                    chain_owner = %account.read().owner(),
                    chain_slot = account.read().slot(),
                    "Received update for undelegating account"
                );

                if account.read().is(AccountMode::Delegated)
                    && ata_projection::derive_eata_pubkey_from_ata_account(
                        &pubkey, &account,
                    )
                    .is_some()
                    && deleg_record.as_ref().is_some_and(|record| {
                        record.owner == EATA_PROGRAM_ID
                            && record.authority == self.validator_pubkey
                    })
                {
                    debug!(
                        pubkey = %pubkey,
                        "Keeping undelegating ATA in bank while companion eATA remains delegated"
                    );
                    return;
                }

                // This will only be true in the following case:
                // 1. a commit was triggered for the account
                // 2. a commit + undelegate was triggered for the account -> undelegating
                // 3. we receive the update for (1.)
                //
                // Thus our state is more up to date and we don't
                // need to update our bank.
                if account_still_undelegating_on_chain(
                    &pubkey,
                    account.read().is(AccountMode::Delegated),
                    slot,
                    deleg_record,
                    &self.validator_pubkey,
                ) {
                    return;
                }
                undelegation_completed_on_chain = true;
            } else if !delegated && account.read().is(AccountMode::Delegated) {
                undelegation_completed_on_chain = true;
            } else if owner == dlp_api::id() {
                debug!(
                    pubkey = %pubkey,
                    "Received update for account owned by delegation program but not marked as undelegating"
                );
            }
        } else {
            debug!(
                pubkey = %pubkey,
                "Received update for account not in bank"
            );
            if account.read().is(AccountMode::Delegated) {
                undelegation_completed_on_chain = true;
            }
        }

        // Determine if delegated to another validator
        let delegated_to_other = deleg_record
            .as_ref()
            .and_then(|dr| self.get_delegated_to_other(dr));

        // Delegated subscription cleanup is limited to direct subscription/pubsub tracking
        // ownership here; undelegation tracking owns protected subscriptions
        // until undelegation is explicitly complete.
        if undelegation_completed_on_chain {
            if !account.read().is(AccountMode::Delegated) {
                self.ensure_direct_subscription_for_completed_account(pubkey)
                    .await;
            }
            self.cleanup_undelegation_tracking_for_completed_account(pubkey)
                .await;
        }
        if account.read().is(AccountMode::Delegated) {
            self.cleanup_direct_subscription_for_delegated_account(pubkey)
                .await;
        }

        if account.read().flags().contains(StateFlags::EXECUTABLE) {
            self.handle_executable_sub_update(
                pubkey,
                account,
                &companion_fetch_log_context,
            )
            .await;
        } else {
            let commit_frequency_ms = deleg_record.as_ref().and_then(|dr| {
                dr.authority
                    .eq(&self.validator_pubkey)
                    .then_some(dr.commit_frequency_ms)
            });
            if let Err(err) = self
                .clone_account_with_post_delegation_action_invariants(
                    AccountCloneRequest {
                        pubkey,
                        account,
                        commit_frequency_ms,
                        post_delegation_mode: ClonePostDelegationMode::from(
                            delegation_actions,
                        ),
                        delegated_to_other,
                    },
                    subscription_clone_context.clone(),
                )
                .await
            {
                error!(
                    pubkey = %pubkey,
                    error = %err,
                    "Failed to clone account into bank"
                );
            } else if let Some(projected_ata_clone_request) =
                projected_ata_clone_request
                && let Err(err) = self
                    .clone_projected_ata_request(
                        projected_ata_clone_request,
                        subscription_clone_context,
                    )
                    .await
            {
                error!(
                    pubkey = %pubkey,
                    error = %err,
                    "Failed to clone projected ATA from delegated eATA update"
                );
            }
        }
    }

    fn ensure_delegation_action_dependencies<'a>(
        &'a self,
        pubkey: Pubkey,
        remote_slot: u64,
        delegation: &'a DelegationActions,
        fetch_context: AccountFetchContext,
    ) -> Pin<Box<dyn Future<Output = ChainlinkResult<()>> + Send + 'a>> {
        Box::pin(async move {
            self.validate_post_delegation_action_signers(delegation.actions())
                .await?;

            let mut dependencies = HashSet::new();
            let mut writable_dependencies = HashSet::new();
            for (dependency, writable) in delegation.dependencies(pubkey) {
                dependencies.insert(dependency);
                if writable {
                    writable_dependencies.insert(dependency);
                }
            }

            let dependencies_to_fetch = {
                let accessor = self.engine.accounts();
                let loader = accessor.loader();
                dependencies
                    .into_iter()
                    .try_fold(Vec::new(), |mut pending, dependency| {
                        let local = loader
                            .read(&dependency, |account| {
                                (account.slot(), account.mode())
                            })
                            .map_err(KeeperError::from)?;
                        if Self::action_dependency_needs_fetch(
                            local,
                            remote_slot,
                            writable_dependencies.contains(&dependency),
                        ) {
                            pending.push(dependency);
                        }
                        Ok::<_, KeeperError>(pending)
                    })
                    .map_err(ChainlinkError::from)?
            };

            if dependencies_to_fetch.is_empty() {
                return Ok(());
            }

            let result = self
                .fetch_and_clone_accounts_with_dedup_forced_refresh(
                    &dependencies_to_fetch,
                    None,
                    Some(remote_slot),
                    fetch_context.with_reason(
                        AccountFetchReason::ActionDependencyForcedRefresh,
                    ),
                    &writable_dependencies,
                    None,
                )
                .await?;
            if result.missing_delegation_record.is_empty() {
                return Ok(());
            }

            let mut missing_accounts =
                result.pubkeys_missing_delegation_record();
            missing_accounts.sort_unstable();
            Err(ChainlinkError::MissingDelegationActionAccounts(
                missing_accounts,
            ))
        })
    }

    fn action_dependency_needs_fetch(
        local: Option<(u64, AccountMode)>,
        remote_slot: u64,
        writable: bool,
    ) -> bool {
        match local {
            None => !writable,
            Some((slot, mode)) => {
                slot < remote_slot && writable && mode != AccountMode::Delegated
            }
        }
    }

    async fn validate_post_delegation_action_signers(
        &self,
        delegation_actions: &[solana_instruction::Instruction],
    ) -> ChainlinkResult<()> {
        let Some(risk_service) = self.risk_service.as_ref() else {
            return Ok(());
        };

        let strategy = risk_service.check_strategy();
        if !delegation_actions_require_risk_check(strategy, delegation_actions)
        {
            // A suppressed check is a compliance-relevant event, so leave a
            // record that the delegation was activated without a risk query.
            debug!(
                strategy = ?strategy,
                action_programs = ?delegation_actions
                    .iter()
                    .map(|ix| ix.program_id)
                    .collect::<Vec<_>>(),
                "Skipping risk check for post-delegation actions"
            );
            return Ok(());
        }

        let mut signers = delegation_actions
            .iter()
            .flat_map(|instruction| {
                instruction.accounts.iter().filter_map(|meta| {
                    if meta.is_signer {
                        Some(meta.pubkey.to_string())
                    } else {
                        None
                    }
                })
            })
            .collect::<Vec<_>>();
        signers.sort_unstable();
        signers.dedup();

        if signers.is_empty() {
            return Ok(());
        }
        Ok(risk_service.check_addresses(signers).await?)
    }

    async fn clone_projected_ata_request(
        &self,
        request: AccountCloneRequest,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<()> {
        if let Some(true) = self.read_account(&request.pubkey, |account| {
            account.is(AccountMode::Transient)
        }) {
            return Ok(());
        }

        self.clone_account_with_post_delegation_action_invariants(
            request,
            fetch_context.with_reason(AccountFetchReason::AtaProjection),
        )
        .await
    }

    async fn clone_released_collision_candidate(
        &self,
        candidate: ParkedCollisionCandidate,
    ) {
        let fresh_delegated = self
            .read_account(&candidate.pubkey, |account| {
                account.is(AccountMode::Delegated)
                    && account.slot() >= candidate.slot
            })
            .unwrap_or(false);
        if fresh_delegated {
            return;
        }

        let fetch_context = AccountFetchContext::subscription_update(
            AccountFetchReason::SubscriptionUpdateGreedyDiscovery,
        );
        let record_context = fetch_context
            .clone()
            .with_reason(AccountFetchReason::DelegationRecord);
        // The record precheck shares the account fetch's effective slot
        // floor; a lower floor could settle on an older in-flight record
        // result and conservatively re-park the candidate.
        let record_min_context_slot = candidate
            .slot
            .max(self.remote_account_provider.chain_slot());
        let Some((deleg_record, _)) = self
            .fetch_and_parse_delegation_record(
                candidate.pubkey,
                record_min_context_slot,
                record_context.clone(),
                CompanionFetchLogContext {
                    origin: record_context,
                    primary_pubkey: candidate.pubkey,
                    context_slot: candidate.slot,
                },
            )
            .await
        else {
            trace!(
                pubkey = %candidate.pubkey,
                slot = candidate.slot,
                "Released collision candidate has no delegation record"
            );
            return;
        };
        if deleg_record.authority != self.validator_pubkey
            && deleg_record.authority != Pubkey::default()
        {
            metrics::inc_discovered_dlp_update_delegated_elsewhere();
            trace!(
                pubkey = %candidate.pubkey,
                authority = %deleg_record.authority,
                "Ignoring released collision candidate delegated elsewhere"
            );
            return;
        }

        const MAX_RELEASE_CLONE_ATTEMPTS: usize = 3;
        for _ in 0..MAX_RELEASE_CLONE_ATTEMPTS {
            // The candidate may have been parked before local undelegation
            // started. Do not let its later record sighting bypass the normal
            // confirmation check and overwrite protected local state.
            let reader = |in_bank: &AccountSharedData| {
                in_bank.is(AccountMode::Transient)
                    && account_still_undelegating_on_chain(
                        &candidate.pubkey,
                        true,
                        in_bank.slot(),
                        Some(deleg_record),
                        &self.validator_pubkey,
                    )
            };
            if self
                .read_account(&candidate.pubkey, reader)
                .unwrap_or(false)
            {
                trace!(
                    pubkey = %candidate.pubkey,
                    slot = candidate.slot,
                    "Ignoring released collision candidate while local undelegation remains pending"
                );
                return;
            }

            let result = match self
                .fetch_and_clone_accounts_with_dedup_forced_refresh(
                    &[candidate.pubkey],
                    None,
                    Some(candidate.slot),
                    fetch_context.clone(),
                    &HashSet::from([candidate.pubkey]),
                    Some((candidate.pubkey, deleg_record.delegation_slot)),
                )
                .await
            {
                Ok(result) => result,
                Err(err) => {
                    warn!(
                        pubkey = %candidate.pubkey,
                        error = %err,
                        "Failed to clone released colliding delegated account"
                    );
                    return;
                }
            };
            let unresolvable = result
                .not_found_on_chain
                .iter()
                .chain(result.missing_delegation_record.iter())
                .any(|(pubkey, _)| pubkey == &candidate.pubkey);
            if unresolvable {
                return;
            }
            let reader = |account: &AccountSharedData| {
                let settled = account.slot() > candidate.slot
                    || (account.slot() == candidate.slot
                        && account.is(AccountMode::Delegated));
                let stale_transient = account.is(AccountMode::Transient)
                    && deleg_record.delegation_slot <= account.slot();
                (settled, stale_transient)
            };
            let in_bank = self.read_account(&candidate.pubkey, reader);
            let settled = in_bank.is_some_and(|(settled, _)| settled);
            if settled {
                return;
            }
            if in_bank.is_some_and(|(_, stale_transient)| stale_transient) {
                break;
            }
        }

        let preserved = self
            .dlp_collision_tracker
            .lock()
            .preserve_released_candidate(candidate);
        warn!(
            pubkey = %candidate.pubkey,
            slot = candidate.slot,
            preserved,
            "Released collision candidate did not settle; preserving it for a later record sighting"
        );
    }

    async fn maybe_greedily_clone_discovered_delegated_account(
        &self,
        pubkey: Pubkey,
        update: &ForwardedSubscriptionUpdate,
    ) -> bool {
        if self.contains_account(&pubkey) {
            return false;
        }

        let Some(account) = update.account.fresh_account() else {
            return false;
        };

        if account.owner() != &dlp_api::id() {
            return false;
        }

        let discovery_context = AccountFetchContext::subscription_update(
            AccountFetchReason::SubscriptionUpdateGreedyDiscovery,
        );
        let record_context = discovery_context
            .clone()
            .with_reason(AccountFetchReason::DelegationRecord);

        let Some((deleg_record, delegation_actions)) = self
            .fetch_and_parse_delegation_record(
                pubkey,
                account.slot(),
                record_context.clone(),
                CompanionFetchLogContext {
                    origin: record_context,
                    primary_pubkey: pubkey,
                    context_slot: account.slot(),
                },
            )
            .await
        else {
            trace!(
                pubkey = %pubkey,
                slot = account.slot(),
                "Greedy discovery could not resolve delegation record; falling back"
            );
            return false;
        };

        let is_delegated_to_us = deleg_record.authority
            == self.validator_pubkey
            || deleg_record.authority == Pubkey::default();
        if !is_delegated_to_us {
            metrics::inc_discovered_dlp_update_delegated_elsewhere();
            trace!(
                pubkey = %pubkey,
                authority = %deleg_record.authority,
                "Ignoring discovered DLP-owned update delegated elsewhere"
            );
            return true;
        }
        let greedy_ata_pubkeys = delegation::parse_raw_eata_pda(
            &pubkey,
            account.data(),
            deleg_record.owner,
        )
        .map(|(wallet_owner, mint)| {
            ata_projection::derive_supported_ata_pubkeys(&wallet_owner, &mint)
        })
        .unwrap_or_default();
        let mut pubkeys_to_clone =
            Vec::with_capacity(1 + greedy_ata_pubkeys.len());
        pubkeys_to_clone.push(pubkey);
        {
            let accessor = self.engine.accounts();
            let loader = accessor.loader();
            pubkeys_to_clone.extend(greedy_ata_pubkeys.iter().copied().filter(
                |ata_pubkey| !loader.contains(ata_pubkey).unwrap_or(false),
            ));
        }

        // Keep eATA discovery with its candidate base ATAs in one clone batch
        // so the normal ATA projection path runs for the same update.
        let clone_result = if greedy_ata_pubkeys.is_empty() {
            self.fetch_and_clone_accounts_with_dedup_forced_refresh(
                &pubkeys_to_clone,
                None,
                Some(account.slot()),
                discovery_context.clone(),
                &HashSet::new(),
                None,
            )
            .await
        } else {
            self.fetch_and_clone_accounts(
                &pubkeys_to_clone,
                None,
                Some(account.slot()),
                discovery_context.clone(),
            )
            .await
            .map(|batch| batch.result)
        };

        match clone_result {
            Ok(result)
                if result
                    .not_found_on_chain
                    .iter()
                    .all(|(missing_pubkey, _)| missing_pubkey != &pubkey)
                    && result.missing_delegation_record.iter().all(
                        |(missing_pubkey, _)| missing_pubkey != &pubkey,
                    ) =>
            {
                let bank_slot =
                    self.read_account(&pubkey, |in_bank| in_bank.slot());
                if bank_slot.is_none_or(|slot| slot < account.slot()) {
                    trace!(
                        pubkey = %pubkey,
                        bank_slot,
                        update_slot = account.slot(),
                        ?result,
                        "Greedy clone did not materialize a fresh enough account; falling back"
                    );
                    false
                } else if let Some(projected_ata_clone_request) = self
                    .maybe_build_projected_ata_clone_request_from_subscription_update_with_source(
                        pubkey,
                        &AccountBuilder::from(AccountSharedData::from(
                            account.owned(),
                        )),
                        update.source,
                        Some(&deleg_record),
                        delegation_actions.as_ref(),
                        &CompanionFetchLogContext {
                            origin: discovery_context.clone(),
                            primary_pubkey: pubkey,
                            context_slot: account.slot(),
                        },
                    )
                    .await
                {
                    let projected_ata_pubkey =
                        projected_ata_clone_request.pubkey;
                    if let Err(err) = self
                        .clone_projected_ata_request(
                            projected_ata_clone_request,
                            discovery_context.clone(),
                        )
                        .await
                    {
                        warn!(
                            pubkey = %pubkey,
                            error = %err,
                            "Failed to clone projected ATA from greedily discovered delegated eATA"
                        );
                        false
                    } else {
                        trace!(
                            pubkey = %pubkey,
                            ata_pubkey = %projected_ata_pubkey,
                            slot = account.slot(),
                            "Greedily cloned delegated account"
                        );
                        true
                    }
                } else {
                    let cloned_ata_pubkey = {
                        let accessor = self.engine.accounts();
                        let loader = accessor.loader();
                        greedy_ata_pubkeys.iter().copied().find(|ata_pubkey| {
                            loader
                                .read(ata_pubkey, |account_in_bank| {
                                    account_in_bank.slot()
                                        >= account.slot()
                                })
                                .ok()
                                .flatten()
                                .unwrap_or(false)
                        })
                    };
                    if let Some(ata_pubkey) = cloned_ata_pubkey {
                        trace!(
                            pubkey = %pubkey,
                            ata_pubkey = %ata_pubkey,
                            slot = account.slot(),
                            "Greedily cloned delegated account"
                        );
                    } else {
                        trace!(
                            pubkey = %pubkey,
                            slot = account.slot(),
                            "Greedily cloned delegated account"
                        );
                    }
                    true
                }
            }
            Ok(result) => {
                trace!(
                    pubkey = %pubkey,
                    ?result,
                    "Greedy clone incomplete; falling back"
                );
                false
            }
            Err(err) => {
                warn!(
                    pubkey = %pubkey,
                    error = %err,
                    "Failed to greedily clone discovered delegated account"
                );
                false
            }
        }
    }

    async fn handle_executable_sub_update(
        &self,
        pubkey: Pubkey,
        account: AccountBuilder,
        companion_fetch_log_context: &CompanionFetchLogContext,
    ) {
        // moved to program_loader module
        program_loader::handle_executable_sub_update_with_context(
            self,
            pubkey,
            account,
            companion_fetch_log_context,
        )
        .await;
    }

    async fn cleanup_direct_subscription_for_delegated_account(
        &self,
        pubkey: Pubkey,
    ) {
        if let Err(err) = self
            .remote_account_provider
            .release_subscription_reason_silently_for_delegated_account(
                &pubkey,
                SubscriptionReason::DirectAccount,
            )
            .await
        {
            warn!(
                pubkey = %pubkey,
                error = %err,
                "Failed to clean up direct subscription for delegated account"
            );
        }
    }

    async fn ensure_direct_subscription_for_completed_account(
        &self,
        pubkey: Pubkey,
    ) {
        if let Err(err) = self
            .remote_account_provider
            .ensure_subscription(&pubkey, SubscriptionReason::DirectAccount)
            .await
        {
            warn!(
                pubkey = %pubkey,
                error = %err,
                "Failed to retain direct subscription for completed account"
            );
        }
    }

    async fn cleanup_undelegation_tracking_for_completed_account(
        &self,
        pubkey: Pubkey,
    ) {
        if let Err(err) = self
            .remote_account_provider
            .release_subscription_reason_silently_for_delegated_account(
                &pubkey,
                SubscriptionReason::UndelegationTracking,
            )
            .await
        {
            warn!(
                pubkey = %pubkey,
                error = %err,
                "Failed to clean up undelegation tracking for completed account"
            );
        }
    }

    async fn resolve_account_to_clone_from_forwarded_sub_with_unsubscribe(
        &self,
        update: ForwardedSubscriptionUpdate,
        companion_fetch_log_context: &CompanionFetchLogContext,
    ) -> (
        Option<AccountBuilder>,
        Option<DelegationRecord>,
        Option<DelegationActions>,
    ) {
        let ForwardedSubscriptionUpdate {
            pubkey,
            account,
            source: _,
        } = update;
        let owned_by_delegation_program =
            account.is_owned_by_delegation_program();

        if let Some(account) = account.into_fresh_account() {
            // If the account is owned by the delegation program we need to resolve
            // its true owner and determine if it is delegated to us
            if owned_by_delegation_program {
                let delegation_record_pubkey =
                    delegation_record_pda_from_delegated_account(&pubkey);

                let acquired_delegation_record_reason = self
                    .acquire_subscription_reason(
                        &delegation_record_pubkey,
                        SubscriptionReason::DelegationRecord,
                    )
                    .await
                    .map(|_| true)
                    .unwrap_or_else(|err| {
                        warn!(
                            pubkey = %delegation_record_pubkey,
                            error = ?err,
                            "Failed to acquire delegation record subscription reason"
                        );
                        false
                    });

                match self
                    .task_to_fetch_with_companion(
                        pubkey,
                        delegation_record_pubkey,
                        account.slot(),
                        AccountFetchContext::subscription_update(
                            AccountFetchReason::DelegationRecord,
                        ),
                        ChainlinkCompanionFetchKind::DelegationRecord,
                    )
                    .await
                {
                    Ok(Ok(AccountWithCompanion {
                        pubkey,
                        account,
                        companion_pubkey: delegation_record_pubkey,
                        companion_account: delegation_record,
                    })) => {
                        // We may need to remove temporary subscriptions created
                        // while resolving this update.
                        let mut subs_to_remove = Vec::new();

                        subs_to_remove.push(SubscriptionRelease::Pubkey {
                            pubkey: delegation_record_pubkey,
                            reason: SubscriptionReason::DirectAccount,
                        });
                        if acquired_delegation_record_reason {
                            subs_to_remove.push(SubscriptionRelease::Pubkey {
                                pubkey: delegation_record_pubkey,
                                reason: SubscriptionReason::DelegationRecord,
                            });
                        }

                        let account = if let Some(delegation_record) =
                            delegation_record
                        {
                            let delegation_record_with_actions = match self
                                .parse_delegation_record(
                                    delegation_record.read().data(),
                                    delegation_record_pubkey,
                                ) {
                                Ok(x) => Some(x),
                                Err(err) => {
                                    error!(
                                        pubkey = %pubkey,
                                        error = %err,
                                        "Failed to parse delegation record"
                                    );
                                    None
                                }
                            };

                            // If the delegation record is valid we set the owner and delegation
                            // status on the account
                            if let Some((
                                delegation_record,
                                delegation_actions,
                            )) = delegation_record_with_actions
                            {
                                if tracing::enabled!(tracing::Level::TRACE) {
                                    let delegation_record_display =
                                        format!("{:?}", delegation_record);
                                    trace!(
                                        pubkey = %pubkey,
                                        slot = account.read().slot(),
                                        owner = %delegation_record.owner,
                                        deleg_record = %delegation_record_display,
                                        "Resolving delegated account"
                                    );
                                }

                                let account = self
                                    .apply_delegation_record_to_account(
                                        pubkey,
                                        account,
                                        &delegation_record,
                                    )
                                    .0;

                                // For accounts delegated to us, subscribe to the original owner
                                // program for undelegation update resilience.
                                if account
                                    .read()
                                    .is(AccountMode::Delegated)
                                    && !self.program_subscription_is_too_broad(
                                        &delegation_record.owner,
                                    )
                                {
                                    // Fire-and-forget to avoid blocking subscription updates.
                                    let provider =
                                        self.remote_account_provider.clone();
                                    let owner = delegation_record.owner;
                                    tokio::spawn(async move {
                                        if let Err(err) = provider
                                            .subscribe_program(owner)
                                            .await
                                        {
                                            warn!(
                                                "Failed to subscribe to owner program {} for account {}: {}",
                                                owner, pubkey, err
                                            );
                                        }
                                    });
                                }

                                (
                                    Some(account),
                                    Some(delegation_record),
                                    delegation_actions,
                                )
                            } else {
                                // If the delegation record is invalid we cannot clone the account
                                // since something is corrupt and we wouldn't know what owner to
                                // use, etc.
                                (None, None, None)
                            }
                        } else if let Ok(request) =
                            UndelegationRequest::try_from_bytes_with_discriminator(
                                account.read().data(),
                            )
                        {
                            let observed = ObservedUndelegationRequest {
                                request_pda: pubkey,
                                delegated_account: request.delegated_account,
                                expires_at_slot: request.expires_at_slot,
                                observed_slot: account.read().slot(),
                            };
                            trace!(
                                request_pda = %observed.request_pda,
                                delegated_account = %observed.delegated_account,
                                expires_at_slot = observed.expires_at_slot,
                                "Observed DLP undelegation request"
                            );
                            if let Err(broadcast::error::SendError(observed)) =
                                self.undelegation_request_sender.send(observed)
                            {
                                warn!(
                                    request_pda = %observed.request_pda,
                                    delegated_account = %observed.delegated_account,
                                    observed_slot = observed.observed_slot,
                                    expires_at_slot = observed.expires_at_slot,
                                    drop_reason = "no_active_subscribers",
                                    "Dropped observed DLP undelegation request because no subscribers are active"
                                );
                            }
                            (
                                Some(account),
                                None,
                                None,
                            )
                        } else if is_internal_dlp_account_data(
                            account.read().data(),
                        ) {
                            (
                                Some(account),
                                None,
                                None,
                            )
                        } else {
                            trace!(
                                pubkey = %pubkey,
                                "Skipping DLP-owned subscription update without delegation record"
                            );
                            (None, None, None)
                        };

                        if !subs_to_remove.is_empty() {
                            release_subs(
                                &self.remote_account_provider,
                                subs_to_remove,
                            )
                            .await;
                        }
                        account
                    }
                    // In case of errors fetching the delegation record we cannot clone the account
                    Ok(Err(err)) => {
                        log_companion_fetch_failure(
                            companion_fetch_log_context,
                            delegation_record_pubkey,
                            ChainlinkCompanionFetchKind::DelegationRecord,
                            &err,
                        );
                        if acquired_delegation_record_reason {
                            release_subs(
                                &self.remote_account_provider,
                                [SubscriptionRelease::Pubkey {
                                    pubkey: delegation_record_pubkey,
                                    reason:
                                        SubscriptionReason::DelegationRecord,
                                }],
                            )
                            .await;
                        }
                        (None, None, None)
                    }
                    Err(err) => {
                        log_companion_fetch_failure(
                            companion_fetch_log_context,
                            delegation_record_pubkey,
                            ChainlinkCompanionFetchKind::DelegationRecord,
                            &err,
                        );
                        if acquired_delegation_record_reason {
                            release_subs(
                                &self.remote_account_provider,
                                [SubscriptionRelease::Pubkey {
                                    pubkey: delegation_record_pubkey,
                                    reason:
                                        SubscriptionReason::DelegationRecord,
                                }],
                            )
                            .await;
                        }
                        (None, None, None)
                    }
                }
            } else {
                let (account, deleg_record) = self
                    .maybe_project_ata_from_subscription_update(
                        pubkey,
                        AccountBuilder::from(account),
                        companion_fetch_log_context,
                    )
                    .await;
                if let Some((deleg_record, actions)) = deleg_record {
                    (Some(account), Some(deleg_record), actions)
                } else {
                    (Some(account), None, None)
                }
            }
        } else {
            // This should not happen since we call this method with sub updates which always hold
            // a fresh remote account
            error!(pubkey = %pubkey, "BUG: Received subscription update without fresh account");
            (None, None, None)
        }
    }

    async fn maybe_build_projected_ata_clone_request_from_subscription_update_with_source(
        &self,
        eata_pubkey: Pubkey,
        eata_account: &AccountBuilder,
        update_source: SubscriptionSource,
        deleg_record: Option<&DelegationRecord>,
        delegation_actions: Option<&DelegationActions>,
        companion_fetch_log_context: &CompanionFetchLogContext,
    ) -> Option<AccountCloneRequest> {
        ata_projection::maybe_build_projected_ata_clone_request_from_subscription_update(
            self,
            eata_pubkey,
            eata_account,
            update_source,
            deleg_record,
            delegation_actions,
            companion_fetch_log_context,
        )
        .await
    }

    async fn maybe_project_ata_from_subscription_update(
        &self,
        ata_pubkey: Pubkey,
        ata_account: AccountBuilder,
        companion_fetch_log_context: &CompanionFetchLogContext,
    ) -> (
        AccountBuilder,
        Option<(DelegationRecord, Option<DelegationActions>)>,
    ) {
        ata_projection::maybe_project_ata_from_subscription_update(
            self,
            ata_pubkey,
            ata_account,
            companion_fetch_log_context,
        )
        .await
    }

    /// Parses a delegation record from account data bytes.
    /// Returns the parsed DelegationRecord, or InvalidDelegationRecord error
    /// if parsing fails.
    fn parse_delegation_record(
        &self,
        data: &[u8],
        delegation_record_pubkey: Pubkey,
    ) -> ChainlinkResult<(DelegationRecord, Option<DelegationActions>)> {
        delegation::parse_delegation_record(
            data,
            delegation_record_pubkey,
            self.validator_keypair.as_ref(),
        )
    }

    /// Applies delegation record settings to an account: sets the owner,
    /// delegation status, and confined status based on the delegation
    /// record's authority field.
    /// Returns commit frequency if account is delegated to us
    fn apply_delegation_record_to_account(
        &self,
        account_pubkey: Pubkey,
        account: AccountBuilder,
        delegation_record: &DelegationRecord,
    ) -> (AccountBuilder, Option<u64>) {
        delegation::apply_delegation_record_to_account(
            self,
            account_pubkey,
            account,
            delegation_record,
        )
    }

    /// Returns the pubkey of another validator if account is delegated to them,
    /// None if delegated to us or delegated to the system program (confined).
    fn get_delegated_to_other(
        &self,
        delegation_record: &DelegationRecord,
    ) -> Option<Pubkey> {
        delegation::get_delegated_to_other(self, delegation_record)
    }

    /// Fetches and parses the delegation record for an account, returning the
    /// parsed DelegationRecord if found and valid, None otherwise.
    async fn fetch_and_parse_delegation_record(
        &self,
        account_pubkey: Pubkey,
        min_context_slot: u64,
        fetch_context: metrics::AccountFetchContext,
        companion_fetch_log_context: CompanionFetchLogContext,
    ) -> Option<(DelegationRecord, Option<DelegationActions>)> {
        delegation::fetch_and_parse_delegation_record(
            self,
            account_pubkey,
            min_context_slot,
            fetch_context,
            &companion_fetch_log_context,
        )
        .await
    }

    async fn fetch_and_clone_accounts(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<FetchAndCloneBatchResult> {
        let accs = match self
            .fetch_accounts(
                pubkeys,
                mark_empty_if_not_found,
                slot,
                fetch_context.clone(),
            )
            .await
        {
            Ok(accs) => accs,
            Err(err) => {
                for _ in pubkeys {
                    metrics::inc_chainlink_clone_accounts_total_with_context(
                        fetch_context.clone(),
                        ChainlinkCloneRemoteResult::Failed,
                        ChainlinkCloneIntent::Unknown,
                        ChainlinkCloneOutcome::Skipped,
                    );
                }
                return Err(err);
            }
        };
        self.clone_accounts(
            pubkeys,
            accs,
            mark_empty_if_not_found,
            slot,
            fetch_context,
        )
        .await
    }

    #[instrument(skip(self, pubkeys), fields(tx_sig = tracing::field::Empty))]
    async fn fetch_accounts(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<Vec<RemoteAccount>> {
        if let Some(sig) = fetch_context.signature() {
            tracing::Span::current().record("tx_sig", sig.to_string());
        }
        if tracing::enabled!(tracing::Level::TRACE) {
            let pubkeys_count = pubkeys.len();
            trace!(count = pubkeys_count, "Fetching accounts");
        }

        // Increment fetch counter for testing deduplication (count per account being fetched)
        self.fetch_count
            .fetch_add(pubkeys.len() as u64, Ordering::Relaxed);

        // Keep the main account fetch aligned with the freshest observed slot.
        let min_context_slot = slot.map(|subscription_slot| {
            subscription_slot.max(self.remote_account_provider.chain_slot())
        });

        let accs = self
            .remote_account_provider
            .try_get_multi(
                pubkeys,
                mark_empty_if_not_found,
                fetch_context,
                min_context_slot,
            )
            .await?;

        if tracing::enabled!(tracing::Level::TRACE) {
            let accs_count = accs.len();
            trace!(count = accs_count, "Fetched accounts");
        }
        Ok(accs)
    }

    #[instrument(skip(self, pubkeys, accs), fields(tx_sig = tracing::field::Empty))]
    async fn clone_accounts(
        &self,
        pubkeys: &[Pubkey],
        accs: Vec<RemoteAccount>,
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<FetchAndCloneBatchResult> {
        if let Some(sig) = fetch_context.signature() {
            tracing::Span::current().record("tx_sig", sig.to_string());
        }

        // Keep resolution fetches aligned with the freshest observed slot.
        let min_context_slot = slot.map(|subscription_slot| {
            subscription_slot.max(self.remote_account_provider.chain_slot())
        });

        let ClassifiedAccounts {
            not_found,
            plain,
            owned_by_deleg,
            programs,
            atas,
        } = pipeline::classify_remote_accounts(accs, pubkeys);

        if tracing::enabled!(tracing::Level::TRACE) {
            let not_found = not_found
                .iter()
                .map(|(pubkey, slot)| (pubkey.to_string(), *slot))
                .collect::<Vec<_>>();
            let plain = plain
                .iter()
                .map(|p| p.pubkey.to_string())
                .collect::<Vec<_>>();
            let owned_by_deleg = owned_by_deleg
                .iter()
                .map(|(pubkey, _, slot)| (pubkey.to_string(), *slot))
                .collect::<Vec<_>>();
            let programs = programs
                .iter()
                .map(|(p, _, _)| p.to_string())
                .collect::<Vec<_>>();
            let atas = atas
                .iter()
                .map(|(a, _, _, _)| a.to_string())
                .collect::<Vec<_>>();
            trace!(
                "Fetched accounts: \nnot_found:      {not_found:?} \nplain:          {plain:?} \nowned_by_deleg: {owned_by_deleg:?}\nprograms:       {programs:?} \natas:       {atas:?}",
            );
        }

        let PartitionedNotFound {
            clone_as_empty,
            not_found,
        } = pipeline::partition_not_found(mark_empty_if_not_found, not_found);

        // We mark some accounts as empty if we know that they will never exist on chain
        if tracing::enabled!(tracing::Level::TRACE)
            && !clone_as_empty.is_empty()
        {
            trace!(
                "Cloning accounts as empty: {:?}",
                clone_as_empty
                    .iter()
                    .map(|(p, _)| p.to_string())
                    .collect::<Vec<_>>()
            );
        }

        // For potentially delegated accounts we update the owner and delegation state first
        let ResolvedDelegatedAccounts {
            mut accounts_to_clone,
            mut record_subs,
            missing_delegation_record,
        } = match pipeline::resolve_delegated_accounts(
            self,
            owned_by_deleg,
            plain,
            min_context_slot,
            fetch_context.clone(),
        )
        .await
        {
            Ok(resolved) => resolved,
            Err(err) => {
                release_subs(
                    &self.remote_account_provider,
                    pubkeys.iter().copied().map(|pubkey| {
                        SubscriptionRelease::Pubkey {
                            pubkey,
                            reason: SubscriptionReason::DirectAccount,
                        }
                    }),
                )
                .await;
                return Err(err);
            }
        };

        let ResolvedPrograms {
            loaded_programs,
            mut program_data_subs,
        } = match pipeline::resolve_programs_with_program_data(
            self,
            programs,
            min_context_slot,
            fetch_context.clone(),
        )
        .await
        {
            Ok(resolved) => resolved,
            Err(err) => {
                let releases = pubkeys
                    .iter()
                    .copied()
                    .map(|pubkey| SubscriptionRelease::Pubkey {
                        pubkey,
                        reason: SubscriptionReason::DirectAccount,
                    })
                    .chain(record_subs.iter().copied().map(|pubkey| {
                        SubscriptionRelease::Pubkey {
                            pubkey,
                            reason: SubscriptionReason::DirectAccount,
                        }
                    }))
                    .chain(record_subs.iter().copied().map(|pubkey| {
                        SubscriptionRelease::Pubkey {
                            pubkey,
                            reason: SubscriptionReason::DelegationRecord,
                        }
                    }))
                    .collect::<Vec<_>>();
                release_subs(&self.remote_account_provider, releases).await;
                return Err(err);
            }
        };

        let mut loaded_programs = loaded_programs;
        let mut all_requested_pubkeys = pubkeys.to_vec();
        all_requested_pubkeys.extend(record_subs.iter().copied());
        all_requested_pubkeys.extend(program_data_subs.iter().copied());

        // We will compute subscription cancellations after ATA handling, once accounts_to_clone is finalized

        // Handle ATAs: for each detected ATA, we derive the eATA PDA, subscribe to both,
        // and, if the ATA is delegated to us and the eATA exists, we clone the eATA data
        // into the ATA in the bank.
        // eATA subscriptions are kept implicitly (not tracked for release).
        let ata_accounts = ata_projection::resolve_ata_with_eata_projection(
            self,
            atas,
            min_context_slot,
            fetch_context.clone(),
        )
        .await;
        accounts_to_clone.extend(ata_accounts);

        // Prefetch absent read-only action dependencies. Absent writable
        // accounts remain available for explicit creation by PostFinalize.
        let action_dependencies =
            pipeline::collect_delegation_action_dependencies(
                &accounts_to_clone,
            );
        let action_dependencies_to_fetch = {
            let accessor = self.engine.accounts();
            let loader = accessor.loader();
            action_dependencies
                .into_iter()
                .try_fold(Vec::new(), |mut pending, (dependency, writable)| {
                    if !writable
                        && !accounts_to_clone
                            .iter()
                            .any(|request| request.pubkey == dependency)
                        && !loaded_programs
                            .iter()
                            .any(|program| program.program_id == dependency)
                        && !loader
                            .contains(&dependency)
                            .map_err(KeeperError::from)?
                    {
                        pending.push(dependency);
                    }
                    Ok::<_, KeeperError>(pending)
                })
                .map_err(ChainlinkError::from)?
        };

        if !action_dependencies_to_fetch.is_empty() {
            if tracing::enabled!(tracing::Level::TRACE) {
                trace!(
                    dependencies = ?action_dependencies_to_fetch,
                    "Ensuring delegation action dependencies"
                );
            }

            self.fetch_count.fetch_add(
                action_dependencies_to_fetch.len() as u64,
                Ordering::Relaxed,
            );
            let action_dependency_context = fetch_context
                .clone()
                .with_reason(AccountFetchReason::ActionDependencyMissing);
            let action_dep_accs = self
                .remote_account_provider
                .try_get_multi(
                    &action_dependencies_to_fetch,
                    None,
                    action_dependency_context.clone(),
                    min_context_slot,
                )
                .await?;
            all_requested_pubkeys
                .extend(action_dependencies_to_fetch.iter().copied());

            let ClassifiedAccounts {
                not_found,
                plain,
                owned_by_deleg,
                programs,
                atas,
            } = pipeline::classify_remote_accounts(
                action_dep_accs,
                &action_dependencies_to_fetch,
            );

            if tracing::enabled!(tracing::Level::TRACE) && !not_found.is_empty()
            {
                trace!(
                    dependencies = ?not_found,
                    "Delegation action dependencies not found on chain; continuing clone flow"
                );
            }

            let ResolvedDelegatedAccounts {
                accounts_to_clone: action_dep_accounts_to_clone,
                record_subs: action_dep_record_subs,
                missing_delegation_record: action_dep_missing_delegation_record,
            } = match pipeline::resolve_delegated_accounts(
                self,
                owned_by_deleg,
                plain,
                min_context_slot,
                action_dependency_context.clone(),
            )
            .await
            {
                Ok(resolved) => resolved,
                Err(err) => {
                    let releases = pipeline::compute_subscription_releases(
                        &all_requested_pubkeys,
                        accounts_to_clone.iter(),
                        &loaded_programs,
                        record_subs.iter().copied(),
                        &program_data_subs,
                    );
                    release_subs(&self.remote_account_provider, releases).await;
                    return Err(err);
                }
            };

            if !action_dep_missing_delegation_record.is_empty() {
                let releases = pipeline::compute_subscription_releases(
                    &all_requested_pubkeys,
                    accounts_to_clone.iter(),
                    &loaded_programs,
                    record_subs
                        .iter()
                        .copied()
                        .chain(action_dep_record_subs.iter().copied()),
                    &program_data_subs,
                );
                release_subs(&self.remote_account_provider, releases).await;
                return Err(ChainlinkError::MissingDelegationActionAccounts(
                    action_dep_missing_delegation_record
                        .iter()
                        .map(|(pubkey, _)| *pubkey)
                        .collect(),
                ));
            }

            all_requested_pubkeys
                .extend(action_dep_record_subs.iter().copied());
            record_subs.extend(action_dep_record_subs);

            let ResolvedPrograms {
                loaded_programs: action_dep_loaded_programs,
                program_data_subs: action_dep_program_data_subs,
            } = match pipeline::resolve_programs_with_program_data(
                self,
                programs,
                min_context_slot,
                action_dependency_context.clone(),
            )
            .await
            {
                Ok(resolved) => resolved,
                Err(err) => {
                    let releases = pipeline::compute_subscription_releases(
                        &all_requested_pubkeys,
                        accounts_to_clone
                            .iter()
                            .chain(&action_dep_accounts_to_clone),
                        &loaded_programs,
                        record_subs.iter().copied(),
                        &program_data_subs,
                    );
                    release_subs(&self.remote_account_provider, releases).await;
                    return Err(err);
                }
            };

            all_requested_pubkeys
                .extend(action_dep_program_data_subs.iter().copied());
            program_data_subs.extend(action_dep_program_data_subs);

            let action_dep_ata_accounts =
                ata_projection::resolve_ata_with_eata_projection(
                    self,
                    atas,
                    min_context_slot,
                    action_dependency_context,
                )
                .await;

            accounts_to_clone.extend(action_dep_accounts_to_clone);
            accounts_to_clone.extend(action_dep_ata_accounts);
            loaded_programs.extend(action_dep_loaded_programs);
        }

        let releases = pipeline::compute_subscription_releases(
            &all_requested_pubkeys,
            accounts_to_clone.iter(),
            &loaded_programs,
            record_subs,
            &program_data_subs,
        );

        pipeline::clone_accounts_and_programs(
            self,
            accounts_to_clone,
            loaded_programs,
            fetch_context,
        )
        .await?;

        release_subs(&self.remote_account_provider, releases).await;

        Ok(FetchAndCloneBatchResult {
            result: FetchAndCloneResult {
                not_found_on_chain: not_found,
                missing_delegation_record,
            },
        })
    }

    /// Determines if the account finished undelegating on chain.
    /// If it has finished undelegating, we should refresh it in the bank.
    /// - **pubkey**: the account pubkey
    /// - **in_bank**: the account as it exists in the bank
    ///
    /// Returns true if the account should be refreshed in the bank
    async fn should_refresh_undelegating_in_bank_account(
        &self,
        pubkey: &Pubkey,
        in_bank_slot: u64,
        delegated: bool,
        eata_pubkey: Option<Pubkey>,
        fetch_context: AccountFetchContext,
    ) -> RefreshDecision {
        {
            debug!(
                pubkey = %pubkey,
                delegated,
                undelegating = true,
                "Fetching undelegating account"
            );

            if let Some(eata_pubkey) = eata_pubkey {
                let undelegating_refresh_context = fetch_context
                    .clone()
                    .with_reason(AccountFetchReason::UndelegatingRefresh);
                let companion_fetch_log_context = CompanionFetchLogContext {
                    origin: undelegating_refresh_context.clone(),
                    primary_pubkey: eata_pubkey,
                    context_slot: self.remote_account_provider.chain_slot(),
                };
                let projected_deleg_record = self
                    .fetch_and_parse_delegation_record(
                        eata_pubkey,
                        self.remote_account_provider.chain_slot(),
                        undelegating_refresh_context,
                        companion_fetch_log_context,
                    )
                    .await;
                if projected_deleg_record.as_ref().is_some_and(|(record, _)| {
                    record.owner == EATA_PROGRAM_ID
                        && record.authority == self.validator_pubkey
                }) {
                    debug!(
                        pubkey = %pubkey,
                        eata_pubkey = %eata_pubkey,
                        "Keeping undelegating ATA in bank while companion eATA remains delegated"
                    );
                    return RefreshDecision::No;
                }
            }

            let undelegating_refresh_context = fetch_context
                .clone()
                .with_reason(AccountFetchReason::UndelegatingRefresh);
            let companion_fetch_log_context = CompanionFetchLogContext {
                origin: undelegating_refresh_context.clone(),
                primary_pubkey: *pubkey,
                context_slot: self.remote_account_provider.chain_slot(),
            };
            let deleg_record = self
                .fetch_and_parse_delegation_record(
                    *pubkey,
                    self.remote_account_provider.chain_slot(),
                    undelegating_refresh_context,
                    companion_fetch_log_context,
                )
                .await;

            if deleg_record.is_none() {
                // If there is no delegation record then it is possible that the account itself
                // does not exist either.
                // In that case we need to refresh it as empty to clear the undelegation state.
                return RefreshDecision::YesAndMarkEmptyIfNotFound;
            }

            let delegated_on_chain =
                deleg_record.as_ref().is_some_and(|(dr, _)| {
                    dr.authority.eq(&self.validator_pubkey)
                        || dr.authority.eq(&Pubkey::default())
                });
            let deleg_record = deleg_record.map(|el| el.0);
            if !account_still_undelegating_on_chain(
                pubkey,
                delegated_on_chain,
                in_bank_slot,
                deleg_record,
                &self.validator_pubkey,
            ) {
                debug!(
                    "Account {pubkey} marked as undelegating will be overridden since undelegation completed"
                );
                return RefreshDecision::Yes;
            }
        }
        RefreshDecision::No
    }

    /// Fetches and clones accounts while the engine serializes mutations for
    /// each target account.
    #[instrument(skip(self, pubkeys))]
    pub async fn fetch_and_clone_accounts_with_dedup(
        &self,
        pubkeys: &[Pubkey],
        slot: Option<u64>,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        self.fetch_and_clone_accounts_with_dedup_forced_refresh(
            pubkeys,
            Some(pubkeys),
            slot,
            fetch_context,
            &HashSet::new(),
            None,
        )
        .await
    }

    async fn fetch_and_clone_accounts_with_dedup_forced_refresh(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
        fetch_context: AccountFetchContext,
        force_refresh_pubkeys: &HashSet<Pubkey>,
        confirmed_redelegation: Option<(Pubkey, u64)>,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        let mut pubkeys = pubkeys.iter().collect::<Vec<_>>();
        if tracing::enabled!(tracing::Level::TRACE) {
            let count = pubkeys.len();
            trace!(count, "Fetching and cloning accounts with dedup");
        }

        let mut in_bank = HashSet::new();
        let mut extra_mark_empty = Vec::new();
        let mut bank_hit_no_fetch_non_undelegating_count = 0_u64;
        let mut bank_hit_no_fetch_undelegating_still_valid_count = 0_u64;
        let mut bank_hit_no_fetch_undelegating_timeout_count = 0_u64;
        let mut bank_hit_undelegating_refresh_required_count = 0_u64;
        let mut bank_miss_remote_required_count = 0_u64;
        let mut forced_refresh_remote_required_count = 0_u64;

        // Phase 1: Sync bank check — separate undelegating accounts
        // (which need async RPC) from non-undelegating (handled
        // synchronously)
        let mut undelegating_checks = vec![];
        {
            let accessor = self.engine.accounts();
            let loader = accessor.loader();
            for pubkey in pubkeys.iter() {
                if force_refresh_pubkeys.contains(*pubkey) {
                    if let Some(slot) = loader
                        .read(pubkey, |account| {
                            account
                                .is(AccountMode::Transient)
                                .then(|| account.slot())
                        })
                        .ok()
                        .flatten()
                        .flatten()
                    {
                        let can_replace = confirmed_redelegation.is_some_and(
                            |(confirmed_pubkey, delegation_slot)| {
                                confirmed_pubkey == **pubkey
                                    && delegation_slot > slot
                            },
                        );
                        if !can_replace {
                            bank_hit_no_fetch_undelegating_still_valid_count +=
                                1;
                            in_bank.insert(**pubkey);
                            continue;
                        }
                    }
                    forced_refresh_remote_required_count += 1;
                    continue;
                }
                let reader = |account: &AccountSharedData| {
                    if account.is(AccountMode::Transient) {
                        Err((
                            account.slot(),
                            account.is(AccountMode::Delegated),
                            ata_projection::derive_eata_pubkey_from_ata_layout(
                                pubkey,
                                account.data(),
                            ),
                        ))
                    } else {
                        Ok((
                            account.owner().eq(&dlp_api::id()),
                            account.is(AccountMode::Delegated),
                            *account.owner(),
                        ))
                    }
                };
                if let Some(account_in_bank) =
                    loader.read(pubkey, reader).ok().flatten()
                {
                    match account_in_bank {
                        Err(account_in_bank) => {
                            undelegating_checks
                                .push((**pubkey, account_in_bank));
                        }
                        Ok((owned_by_dlp, delegated, owner)) => {
                            if owned_by_dlp {
                                debug!(
                                    pubkey = %pubkey,
                                    "Account owned by deleg program not marked as undelegating"
                                );
                            }
                            if tracing::enabled!(tracing::Level::TRACE) {
                                trace!(
                                    pubkey = %pubkey,
                                    undelegating = false,
                                    delegated,
                                    owner = %owner,
                                    "Account found in bank in valid state, no fetch needed"
                                );
                            }
                            bank_hit_no_fetch_non_undelegating_count += 1;
                            in_bank.insert(**pubkey);
                        }
                    }
                } else {
                    bank_miss_remote_required_count += 1;
                }
            }
        }

        // Phase 2: Parallel undelegation checks via JoinSet
        if !undelegating_checks.is_empty() {
            let mut join_set = JoinSet::new();
            for (pubkey, (slot, delegated, eata_pubkey)) in undelegating_checks
            {
                let this = self.clone();
                let fetch_context = fetch_context.clone();
                join_set.spawn(async move {
                    let decision = match tokio::time::timeout(
                        Duration::from_secs(5),
                        this.should_refresh_undelegating_in_bank_account(
                            &pubkey,
                            slot,
                            delegated,
                            eata_pubkey,
                            fetch_context,
                        ),
                    )
                    .await
                    {
                        Ok(decision) => decision,
                        Err(_timeout) => {
                            warn!(
                                pubkey = %pubkey,
                                "Timeout checking if account is still undelegating after 5 seconds"
                            );
                            return (pubkey, None);
                        }
                    };
                    (pubkey, Some(decision))
                });
            }

            for (pubkey, decision) in join_set.join_all().await {
                match decision {
                    Some(
                        decision @ (RefreshDecision::Yes
                        | RefreshDecision::YesAndMarkEmptyIfNotFound),
                    ) => {
                        debug!(
                            pubkey = %pubkey,
                            "Account completed undelegation which was missed and is fetched again"
                        );
                        bank_hit_undelegating_refresh_required_count += 1;
                        metrics::inc_unstuck_undelegation_count();
                        if let RefreshDecision::YesAndMarkEmptyIfNotFound =
                            decision
                        {
                            extra_mark_empty.push(pubkey);
                        }
                    }
                    Some(RefreshDecision::No) => {
                        if tracing::enabled!(tracing::Level::TRACE) {
                            trace!(
                                pubkey = %pubkey,
                                "Undelegating account still valid, no fetch needed"
                            );
                        }
                        bank_hit_no_fetch_undelegating_still_valid_count += 1;
                        in_bank.insert(pubkey);
                    }
                    None => {
                        bank_hit_no_fetch_undelegating_timeout_count += 1;
                        in_bank.insert(pubkey);
                    }
                }
            }
        }
        metrics::inc_chainlink_bank_precheck_accounts_with_context(
            fetch_context.clone(),
            BankPrecheckOutcome::BankHitNoFetch,
            BankPrecheckReason::NonUndelegatingPresent,
            bank_hit_no_fetch_non_undelegating_count,
        );
        metrics::inc_chainlink_bank_precheck_accounts_with_context(
            fetch_context.clone(),
            BankPrecheckOutcome::BankHitNoFetch,
            BankPrecheckReason::UndelegatingStillValid,
            bank_hit_no_fetch_undelegating_still_valid_count,
        );
        metrics::inc_chainlink_bank_precheck_accounts_with_context(
            fetch_context.clone(),
            BankPrecheckOutcome::BankHitNoFetch,
            BankPrecheckReason::UndelegatingCheckTimeout,
            bank_hit_no_fetch_undelegating_timeout_count,
        );
        metrics::inc_chainlink_bank_precheck_accounts_with_context(
            fetch_context
                .clone()
                .with_reason(AccountFetchReason::UndelegatingRefresh),
            BankPrecheckOutcome::BankHitUndelegatingRefreshRequired,
            BankPrecheckReason::UndelegatingRefresh,
            bank_hit_undelegating_refresh_required_count,
        );
        metrics::inc_chainlink_bank_precheck_accounts_with_context(
            fetch_context.clone(),
            BankPrecheckOutcome::BankMissRemoteRequired,
            BankPrecheckReason::Absent,
            bank_miss_remote_required_count,
        );
        metrics::inc_chainlink_bank_precheck_accounts_with_context(
            fetch_context.clone(),
            BankPrecheckOutcome::ForcedRefreshRemoteRequired,
            BankPrecheckReason::ForcedRefresh,
            forced_refresh_remote_required_count,
        );
        pubkeys.retain(|p| !in_bank.contains(p));

        let mut mark_empty = mark_empty_if_not_found
            .unwrap_or(&[])
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        mark_empty.extend(extra_mark_empty);
        let mark_empty = mark_empty.into_iter().collect::<Vec<_>>();
        let mark_empty =
            (!mark_empty.is_empty()).then_some(mark_empty.as_slice());

        let fetch_pubkeys = pubkeys.into_iter().copied().collect::<Vec<_>>();
        let batch = if fetch_pubkeys.is_empty() {
            FetchAndCloneBatchResult::default()
        } else {
            self.fetch_and_clone_accounts(
                &fetch_pubkeys,
                mark_empty,
                slot,
                fetch_context,
            )
            .await?
        };

        Ok(batch.result)
    }

    fn task_to_fetch_with_delegation_record(
        &self,
        pubkey: Pubkey,
        slot: u64,
        fetch_context: AccountFetchContext,
    ) -> task::JoinHandle<ChainlinkResult<AccountWithCompanion>> {
        let delegation_record_pubkey =
            delegation_record_pda_from_delegated_account(&pubkey);
        self.task_to_fetch_with_companion(
            pubkey,
            delegation_record_pubkey,
            slot,
            fetch_context.with_reason(AccountFetchReason::DelegationRecord),
            ChainlinkCompanionFetchKind::DelegationRecord,
        )
    }

    fn task_to_fetch_with_program_data(
        &self,
        pubkey: Pubkey,
        slot: u64,
        fetch_context: AccountFetchContext,
    ) -> task::JoinHandle<ChainlinkResult<AccountWithCompanion>> {
        let program_data_pubkey =
            get_loaderv3_get_program_data_address(&pubkey);
        self.task_to_fetch_with_companion(
            pubkey,
            program_data_pubkey,
            slot,
            fetch_context.with_reason(AccountFetchReason::ProgramData),
            ChainlinkCompanionFetchKind::ProgramData,
        )
    }

    fn task_to_fetch_with_companion(
        &self,
        pubkey: Pubkey,
        companion_pubkey: Pubkey,
        slot: u64,
        fetch_context: AccountFetchContext,
        companion_fetch_kind: ChainlinkCompanionFetchKind,
    ) -> task::JoinHandle<ChainlinkResult<AccountWithCompanion>> {
        let provider = self.remote_account_provider.clone();
        let engine = self.engine.clone();
        let fetch_count = self.fetch_count.clone();
        task::spawn(async move {
            trace!(
                pubkey = %pubkey,
                companion = %companion_pubkey,
                slot,
                "Fetching account with companion"
            );

            // Increment fetch counter for testing deduplication (2 accounts: pubkey + delegation_record_pubkey)
            fetch_count.fetch_add(2, Ordering::Relaxed);

            provider
                .try_get_multi_until_slots_match(
                    &[pubkey, companion_pubkey],
                    Some(MatchSlotsConfig {
                        min_context_slot: Some(slot),
                        ..MatchSlotsConfig::new(companion_fetch_kind)
                    }),
                    fetch_context,
                )
                .await
                .map_err(ChainlinkError::from)
                .and_then(|accs| {
                    match accs.as_slice() {
                        [acc_first, acc_last] => {
                            Ok((acc_first.clone(), acc_last.clone()))
                        }
                        _ => Err(ChainlinkError::UnexpectedAccountCount(format!(
                            "Expected exactly 2 accounts for pubkey {} and companion {}, got {}",
                            pubkey,
                            companion_pubkey,
                            accs.len()
                        ))),
                    }
                })
                .and_then(|(acc, deleg)| {
                    Self::resolve_account_with_companion(
                        &engine,
                        pubkey,
                        companion_pubkey,
                        acc,
                        deleg,
                    )
                })
        })
    }

    fn resolve_account_with_companion(
        engine: &Engine,
        pubkey: Pubkey,
        companion_pubkey: Pubkey,
        acc: RemoteAccount,
        companion: RemoteAccount,
    ) -> ChainlinkResult<AccountWithCompanion> {
        use RemoteAccount::*;
        let accessor = engine.accounts();
        let loader = accessor.loader();
        let resolve = |account: &ResolvedAccount| match account {
            ResolvedAccount::Fresh(account) => Some(AccountBuilder::from(
                AccountSharedData::from(account.owned()),
            )),
            ResolvedAccount::Bank((pubkey, _)) => loader
                .read(pubkey, |account| {
                    AccountBuilder::from(AccountSharedData::from(
                        account.owned(),
                    ))
                })
                .ok()
                .flatten(),
        };
        match (acc, companion) {
            // Account not found even though we found it previously - this is invalid,
            // either way we cannot use it now
            (NotFound(_), NotFound(_)) | (NotFound(_), Found(_)) => {
                Err(ChainlinkError::ResolvedAccountCouldNoLongerBeFound(pubkey))
            }
            (Found(acc), NotFound(_)) => {
                // Only account found without a companion
                // In case of delegation record fetch the account is either invalid
                // or a delegation record itself.
                // Clone it as is (without changing the owner or flagging as delegated)
                match resolve(&acc.account) {
                    Some(account) => Ok(AccountWithCompanion {
                        pubkey,
                        account,
                        companion_pubkey,
                        companion_account: None,
                    }),
                    None => Err(
                        ChainlinkError::ResolvedAccountCouldNoLongerBeFound(
                            pubkey,
                        ),
                    ),
                }
            }
            (Found(acc), Found(comp)) => {
                // Found the delegation record, we include it so that the caller can
                // use it to add metadata to the account and use it for decision making
                let Some(comp_account) = resolve(&comp.account) else {
                    return Err(
                        ChainlinkError::ResolvedCompanionAccountCouldNoLongerBeFound(
                            companion_pubkey,
                        ),
                    );
                };
                let Some(account) = resolve(&acc.account) else {
                    return Err(
                        ChainlinkError::ResolvedAccountCouldNoLongerBeFound(
                            pubkey,
                        ),
                    );
                };
                Ok(AccountWithCompanion {
                    pubkey,
                    account,
                    companion_pubkey,
                    companion_account: Some(comp_account),
                })
            }
        }
    }

    /// Check if an account is currently being watched (subscribed to) by the
    /// remote account provider
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
