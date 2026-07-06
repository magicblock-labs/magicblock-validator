use std::{
    collections::{HashSet, hash_map},
    future::Future,
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
    try_derive_supported_ata_pubkeys,
};
use magicblock_metrics::metrics::{
    self, AccountFetchContext, AccountFetchReason, BankPrecheckOutcome,
    BankPrecheckReason, ChainlinkCloneIntent, ChainlinkCloneOutcome,
    ChainlinkCloneRemoteResult, ChainlinkCompanionFetchKind,
    ChainlinkEmptyPlaceholderStage, Outcome,
};
use parking_lot::Mutex as PlMutex;
use solana_account::{
    AccountFieldPatch, AccountMode, AccountSharedData, ReadableAccount,
};
use solana_account_decoder_client_types::UiAccountEncoding;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{
    config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    filter::{Memcmp, RpcFilterType},
};
use solana_sdk_ids::system_program;
use solana_signer::Signer;
use tokio::{
    sync::{Semaphore, broadcast, mpsc, oneshot},
    task,
    task::JoinSet,
};
use tracing::*;

mod ata_projection;
mod delegation;
mod pending_clone_guard;
mod pipeline;
mod program_loader;
mod subscription;
#[cfg(test)]
mod tests;
mod types;

pub use self::types::FetchAndCloneResult;
use self::{
    pending_clone_guard::{CloneClaim, CloneCompletion, PendingCloneGuard},
    subscription::{SubscriptionRelease, release_subs},
    types::{
        AccountWithCompanion, ClassifiedAccounts, PartitionedNotFound,
        RefreshDecision, ResolvedDelegatedAccounts, ResolvedPrograms,
    },
};
use super::errors::{ChainlinkError, ChainlinkResult};
use crate::{
    chainlink::{
        ObservedUndelegationRequest,
        account_still_undelegating_on_chain::account_still_undelegating_on_chain,
    },
    cloner::{
        self, AccountCloneRequest, DelegationActions,
        errors::{ClonerError, ClonerResult},
    },
    remote_account_provider::{
        ChainPubsubClient, ChainRpcClient, ForwardedSubscriptionUpdate,
        MatchSlotsConfig, RemoteAccount, RemoteAccountProvider,
        ResolvedAccount, ResolvedAccountSharedData, SubscriptionReason,
        program_account::{
            LoadedProgram, get_loaderv3_get_program_data_address,
        },
        pubsub_common::{SubscriptionSource, is_internal_dlp_account_data},
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

    engine: Engine,
    validator_pubkey: Pubkey,
    validator_keypair: Arc<Keypair>,

    /// If specified, only these programs will be cloned. If None or empty,
    /// all programs are allowed.
    allowed_programs: Option<HashSet<Pubkey>>,

    /// Negative cache for derived eATAs confirmed missing on chain.
    known_empty_eatas: Arc<PlMutex<LruCache<Pubkey, ()>>>,

    /// Tracks in-flight clone operations.
    /// The first caller to claim a key becomes the owner and performs
    /// the actual clone. Subsequent callers become waiters and receive
    /// the result via oneshot channels. Prevents duplicate clone
    /// submissions across concurrent fetch and subscription paths.
    pending_clones: Arc<
        Mutex<hash_map::HashMap<Pubkey, Vec<oneshot::Sender<CloneCompletion>>>>,
    >,

    pending_undelegations: Arc<Mutex<HashSet<Pubkey>>>,

    /// Risk checker for post-delegation action addresses.
    risk_service: Option<Arc<RiskService>>,

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

/// Negative-cache capacity for known-empty eATAs.
const KNOWN_EMPTY_EATAS_CAPACITY: NonZeroUsize =
    match NonZeroUsize::new(100_000) {
        Some(n) => n,
        None => panic!("KNOWN_EMPTY_EATAS_CAPACITY must be non-zero"),
    };

impl<T, U> Clone for FetchCloner<T, U>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
{
    fn clone(&self) -> Self {
        Self {
            remote_account_provider: self.remote_account_provider.clone(),
            fetch_count: self.fetch_count.clone(),
            engine: self.engine.clone(),
            validator_pubkey: self.validator_pubkey,
            validator_keypair: Arc::clone(&self.validator_keypair),
            allowed_programs: self.allowed_programs.clone(),
            known_empty_eatas: self.known_empty_eatas.clone(),
            pending_clones: self.pending_clones.clone(),
            pending_undelegations: self.pending_undelegations.clone(),
            risk_service: self.risk_service.clone(),
            undelegation_request_sender: self
                .undelegation_request_sender
                .clone(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
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
            allowed_programs,
            known_empty_eatas: Arc::new(PlMutex::new(LruCache::new(
                KNOWN_EMPTY_EATAS_CAPACITY,
            ))),
            pending_clones: Arc::new(Mutex::new(hash_map::HashMap::new())),
            pending_undelegations: Arc::new(Mutex::new(HashSet::new())),
            risk_service,
            undelegation_request_sender,
        });

        me.clone()
            .start_subscription_listener(subscription_updates_rx);

        me
    }

    /// Get the current fetch count
    pub fn fetch_count(&self) -> u64 {
        self.fetch_count.load(Ordering::Relaxed)
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

    pub(crate) fn get_account(
        &self,
        pubkey: &Pubkey,
    ) -> Option<AccountSharedData> {
        self.engine.accounts().get(pubkey).ok().flatten()
    }

    pub(crate) fn remote_account_provider(
        &self,
    ) -> &Arc<RemoteAccountProvider<T, U>> {
        &self.remote_account_provider
    }

    /// Returns the number of waiters currently joined to the low-level
    /// clone operation keyed by `pubkey`, or `None` if no clone is pending.
    #[cfg(any(test, feature = "dev-context"))]
    pub fn pending_clone_waiter_count(&self, pubkey: &Pubkey) -> Option<usize> {
        let map = self
            .pending_clones
            .lock()
            .expect("pending_clones mutex poisoned");
        map.get(pubkey).map(Vec::len)
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

    /// Attempt to claim ownership of a clone operation for a clone key.
    /// Returns `CloneClaim::Owner` if this caller is the first and should
    /// perform the clone. Returns `CloneClaim::Waiter(rx)` if another
    /// caller already owns this clone and this caller should wait.
    fn claim_pending_clone(&self, pubkey: Pubkey) -> CloneClaim {
        let mut map = self
            .pending_clones
            .lock()
            .expect("pending_clones mutex poisoned");
        match map.entry(pubkey) {
            hash_map::Entry::Vacant(entry) => {
                entry.insert(Vec::new());
                CloneClaim::Owner
            }
            hash_map::Entry::Occupied(mut entry) => {
                let (tx, rx) = oneshot::channel();
                entry.get_mut().push(tx);
                CloneClaim::Waiter(rx)
            }
        }
    }

    /// Called by the owner when the clone operation completes.
    /// Removes the pending entry and notifies all waiters.
    fn finish_pending_clone(&self, pubkey: Pubkey, result: CloneCompletion) {
        let waiters = {
            let mut map = self
                .pending_clones
                .lock()
                .expect("pending_clones mutex poisoned");
            map.remove(&pubkey).unwrap_or_default()
        };
        for tx in waiters {
            let _ = tx.send(result);
        }
    }

    fn account_is_actively_delegated(account: &AccountSharedData) -> bool {
        account.is(AccountMode::Delegated)
            && !account.is(AccountMode::Transient)
    }

    fn program_subscription_is_too_broad(&self, program_id: &Pubkey) -> bool {
        program_id == &TOKEN_PROGRAM_ID
            || program_id == &spl_token_2022::id()
            || program_id == &ASSOCIATED_TOKEN_PROGRAM_ID
    }

    fn local_account_satisfies_clone_request(
        &self,
        request: &AccountCloneRequest,
    ) -> bool {
        let active_delegation_satisfies_request =
            request.delegation_actions.is_empty()
                && request.delegated_to_other.is_none()
                && !request.needs_undelegation;
        self.get_account(&request.pubkey).is_some_and(|account| {
            let local_slot = account.slot();
            let request_slot = request.account.slot();
            (active_delegation_satisfies_request
                && Self::account_is_actively_delegated(&account))
                || local_slot > request_slot
                || (local_slot == request_slot && account.eq(&request.account))
        })
    }

    fn is_empty_placeholder_account(account: &AccountSharedData) -> bool {
        account.lamports() == 0
            && account.data().is_empty()
            && account.owner() == &Pubkey::default()
            && !account.executable()
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
        } else if request.account.is(AccountMode::Delegated) {
            ChainlinkCloneIntent::DelegationRecord
        } else if !request.delegation_actions.is_empty() {
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

    /// Submits a clone request through ownership coordination.
    /// Only one account clone per pubkey is submitted at a time. Waiters
    /// retry local freshness checks after a successful owner so a newer
    /// request can clone after an older request finishes.
    async fn clone_account_with_ownership(
        &self,
        request: AccountCloneRequest,
        fetch_context: AccountFetchContext,
    ) -> ClonerResult<()> {
        let pubkey = request.pubkey;
        let remote_result = Self::clone_remote_result_for_request(&request);
        let clone_intent = Self::clone_intent_for_request(&request);
        let mut request = Some(request);

        loop {
            let Some(request_ref) = request.as_ref() else {
                return Err(ClonerError::FailedToCloneRegularAccount(
                    pubkey,
                    Box::new(ClonerError::CommittorServiceError(
                        "missing clone request before ownership claim"
                            .to_string(),
                    )),
                ));
            };

            if self.local_account_satisfies_clone_request(request_ref) {
                metrics::inc_chainlink_clone_accounts_total_with_context(
                    fetch_context,
                    remote_result,
                    clone_intent,
                    ChainlinkCloneOutcome::Skipped,
                );
                return Ok(());
            }

            match self.claim_pending_clone(pubkey) {
                CloneClaim::Owner => {
                    let mut guard = PendingCloneGuard::new(
                        Arc::clone(&self.pending_clones),
                        pubkey,
                    );
                    metrics::inc_chainlink_clone_accounts_total_with_context(
                        fetch_context,
                        remote_result,
                        clone_intent,
                        ChainlinkCloneOutcome::Submitted,
                    );
                    let Some(owned_request) = request.take() else {
                        let err = ClonerError::CommittorServiceError(
                            "owner missing request for clone".to_string(),
                        );
                        self.finish_pending_clone(
                            pubkey,
                            CloneCompletion::Failed,
                        );
                        guard.dismiss();
                        return Err(ClonerError::FailedToCloneRegularAccount(
                            pubkey,
                            Box::new(err),
                        ));
                    };
                    let active_delegation_satisfies_request =
                        owned_request.delegation_actions.is_empty()
                            && owned_request.delegated_to_other.is_none()
                            && !owned_request.needs_undelegation;
                    let is_empty_placeholder =
                        Self::is_empty_placeholder_account(
                            &owned_request.account,
                        );
                    Self::record_empty_placeholder_stage(
                        is_empty_placeholder,
                        fetch_context,
                        ChainlinkEmptyPlaceholderStage::CloneSubmitted,
                        Outcome::Success,
                    );
                    let result =
                        cloner::clone_account(&self.engine, owned_request)
                            .await;
                    let reconciled_active_delegation = result.is_err()
                        && active_delegation_satisfies_request
                        && self.get_account(&pubkey).is_some_and(|account| {
                            Self::account_is_actively_delegated(&account)
                        });
                    if reconciled_active_delegation {
                        debug!(
                            pubkey = %pubkey,
                            error = ?result,
                            "Clone request satisfied by concurrently active local delegation"
                        );
                        metrics::inc_chainlink_clone_accounts_total_with_context(
                            fetch_context,
                            remote_result,
                            clone_intent,
                            ChainlinkCloneOutcome::Skipped,
                        );
                    } else if result.is_ok() {
                        metrics::inc_chainlink_clone_accounts_total_with_context(
                            fetch_context,
                            remote_result,
                            clone_intent,
                            ChainlinkCloneOutcome::CloneSucceeded,
                        );
                    } else {
                        metrics::inc_chainlink_clone_accounts_total_with_context(
                            fetch_context,
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
                        Self::record_empty_placeholder_stage(
                            is_empty_placeholder,
                            fetch_context,
                            ChainlinkEmptyPlaceholderStage::CloneSubmitFailed,
                            Outcome::Error,
                        );
                    }
                    let completion =
                        if result.is_ok() || reconciled_active_delegation {
                            CloneCompletion::Success
                        } else {
                            CloneCompletion::Failed
                        };
                    self.finish_pending_clone(pubkey, completion);
                    guard.dismiss();
                    return if reconciled_active_delegation {
                        Ok(())
                    } else {
                        result
                    };
                }
                CloneClaim::Waiter(rx) => match rx.await {
                    Ok(CloneCompletion::Success) => continue,
                    Ok(CloneCompletion::Failed) => {
                        return Err(ClonerError::FailedToCloneRegularAccount(
                            pubkey,
                            Box::new(ClonerError::CommittorServiceError(
                                "Clone owner failed".to_string(),
                            )),
                        ));
                    }
                    Err(_) => {
                        return Err(ClonerError::FailedToCloneRegularAccount(
                            pubkey,
                            Box::new(ClonerError::CommittorServiceError(
                                "Clone owner dropped".to_string(),
                            )),
                        ));
                    }
                },
            }
        }
    }

    async fn clone_program_with_ownership(
        &self,
        program: LoadedProgram,
        fetch_context: AccountFetchContext,
    ) -> ClonerResult<()> {
        let program_id = program.program_id;
        let remote_slot = program.remote_slot;
        let remote_result = ChainlinkCloneRemoteResult::Found;
        let clone_intent = ChainlinkCloneIntent::ProgramData;

        loop {
            if self
                .get_account(&program_id)
                .is_some_and(|account| account.slot() >= remote_slot)
            {
                metrics::inc_chainlink_clone_accounts_total_with_context(
                    fetch_context,
                    remote_result,
                    clone_intent,
                    ChainlinkCloneOutcome::Skipped,
                );
                return Ok(());
            }

            match self.claim_pending_clone(program_id) {
                CloneClaim::Owner => {
                    let mut guard = PendingCloneGuard::new(
                        Arc::clone(&self.pending_clones),
                        program_id,
                    );

                    let result = if self
                        .get_account(&program_id)
                        .is_some_and(|account| account.slot() >= remote_slot)
                    {
                        metrics::inc_chainlink_clone_accounts_total_with_context(
                            fetch_context,
                            remote_result,
                            clone_intent,
                            ChainlinkCloneOutcome::Skipped,
                        );
                        Ok(())
                    } else {
                        metrics::inc_chainlink_clone_accounts_total_with_context(
                            fetch_context,
                            remote_result,
                            clone_intent,
                            ChainlinkCloneOutcome::Submitted,
                        );
                        let result =
                            cloner::clone_program(&self.engine, program).await;
                        if result.is_ok() {
                            metrics::inc_chainlink_clone_accounts_total_with_context(
                                fetch_context,
                                remote_result,
                                clone_intent,
                                ChainlinkCloneOutcome::CloneSucceeded,
                            );
                        } else {
                            metrics::inc_chainlink_clone_accounts_total_with_context(
                                fetch_context,
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
                    };
                    let completion = if result.is_ok() {
                        CloneCompletion::Success
                    } else {
                        CloneCompletion::Failed
                    };
                    self.finish_pending_clone(program_id, completion);
                    guard.dismiss();
                    return result;
                }
                CloneClaim::Waiter(rx) => match rx.await {
                    Ok(CloneCompletion::Success) => continue,
                    Ok(CloneCompletion::Failed) => {
                        return Err(ClonerError::FailedToCloneProgram(
                            program_id,
                            Box::new(ClonerError::CommittorServiceError(
                                "Clone owner failed".to_string(),
                            )),
                        ));
                    }
                    Err(_) => {
                        return Err(ClonerError::FailedToCloneProgram(
                            program_id,
                            Box::new(ClonerError::CommittorServiceError(
                                "Clone owner dropped".to_string(),
                            )),
                        ));
                    }
                },
            }
        }
    }

    async fn clone_account_with_post_delegation_action_invariants(
        &self,
        mut request: AccountCloneRequest,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<()> {
        if request.account.is(AccountMode::Delegated)
            && is_ata(&request.pubkey, &request.account).is_some()
            && !normalize_native_token_account_for_local_clone(
                &mut request.account,
            )
        {
            return Err(ChainlinkError::InvalidTokenAccount(
                request.pubkey,
                "delegated ATA token data is malformed".to_string(),
            ));
        }
        self.normalize_unresolved_dlp_clone_request(&mut request)?;

        if request.account.is(AccountMode::Delegated)
            && self.local_delegated_clone_target_active(request.pubkey)
        {
            return Ok(());
        }

        if request.delegation_actions.is_empty() {
            return Ok(self
                .clone_account_with_ownership(request, fetch_context)
                .await?);
        }

        if !request.account.is(AccountMode::Delegated) {
            return Err(ChainlinkError::InvalidDelegationActions(
                request.pubkey,
                "post-delegation actions attached to non-delegated clone target"
                    .to_string(),
            ));
        }

        let result = async {
            self.ensure_delegation_action_dependencies(
                request.pubkey,
                request.account.slot(),
                &request.delegation_actions,
                fetch_context,
            )
            .await?;

            Ok(self
                .clone_account_with_ownership(request.clone(), fetch_context)
                .await?)
        }
        .await;

        match result {
            Ok(signature) => Ok(signature),
            Err(err) => {
                let pubkey = request.pubkey;
                if self
                    .get_account(&pubkey)
                    .is_some_and(|account| account.is(AccountMode::Transient))
                {
                    return Err(err);
                }

                let Some(_guard) = self.claim_undelegation(pubkey) else {
                    return Err(err);
                };

                // The post-delegation actions could not be satisfied (e.g. a
                // high-risk signer or a missing dependency): the account is
                // still cloned, but flagged so it gets automatically
                // undelegated back to chain.
                match self
                    .clone_account_and_schedule_undelegation_with_ownership(
                        request,
                        fetch_context,
                    )
                    .await
                {
                    Ok(signature) => Ok(signature),
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
        }
    }

    async fn clone_account_and_schedule_undelegation_with_ownership(
        &self,
        mut request: AccountCloneRequest,
        fetch_context: AccountFetchContext,
    ) -> ClonerResult<()> {
        let pubkey = request.pubkey;
        request.needs_undelegation = true;
        let remote_result = Self::clone_remote_result_for_request(&request);
        let clone_intent = Self::clone_intent_for_request(&request);
        let mut request = Some(request);

        loop {
            if self
                .get_account(&pubkey)
                .is_some_and(|account| account.is(AccountMode::Transient))
            {
                metrics::inc_chainlink_clone_accounts_total_with_context(
                    fetch_context,
                    remote_result,
                    clone_intent,
                    ChainlinkCloneOutcome::Skipped,
                );
                return Ok(());
            }

            match self.claim_pending_clone(pubkey) {
                CloneClaim::Owner => {
                    let mut guard = PendingCloneGuard::new(
                        Arc::clone(&self.pending_clones),
                        pubkey,
                    );
                    let Some(owned_request) = request.take() else {
                        let err = ClonerError::CommittorServiceError(
                            "owner missing request for undelegation clone"
                                .to_string(),
                        );
                        self.finish_pending_clone(
                            pubkey,
                            CloneCompletion::Failed,
                        );
                        guard.dismiss();
                        return Err(
                            ClonerError::FailedToCloneAndScheduleUndelegation(
                                pubkey,
                                Box::new(err),
                            ),
                        );
                    };
                    metrics::inc_chainlink_clone_accounts_total_with_context(
                        fetch_context,
                        remote_result,
                        clone_intent,
                        ChainlinkCloneOutcome::Submitted,
                    );
                    let is_empty_placeholder =
                        Self::is_empty_placeholder_account(
                            &owned_request.account,
                        );
                    Self::record_empty_placeholder_stage(
                        is_empty_placeholder,
                        fetch_context,
                        ChainlinkEmptyPlaceholderStage::CloneSubmitted,
                        Outcome::Success,
                    );
                    let result =
                        cloner::clone_account(&self.engine, owned_request)
                            .await;
                    if result.is_ok() {
                        metrics::inc_chainlink_clone_accounts_total_with_context(
                            fetch_context,
                            remote_result,
                            clone_intent,
                            ChainlinkCloneOutcome::CloneSucceeded,
                        );
                    } else {
                        metrics::inc_chainlink_clone_accounts_total_with_context(
                            fetch_context,
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
                        Self::record_empty_placeholder_stage(
                            is_empty_placeholder,
                            fetch_context,
                            ChainlinkEmptyPlaceholderStage::CloneSubmitFailed,
                            Outcome::Error,
                        );
                    }
                    let completion = if result.is_ok() {
                        CloneCompletion::Success
                    } else {
                        CloneCompletion::Failed
                    };
                    self.finish_pending_clone(pubkey, completion);
                    guard.dismiss();
                    return result;
                }
                CloneClaim::Waiter(rx) => match rx.await {
                    Ok(CloneCompletion::Success) => continue,
                    Ok(CloneCompletion::Failed) => {
                        return Err(ClonerError::FailedToCloneRegularAccount(
                            pubkey,
                            Box::new(ClonerError::CommittorServiceError(
                                "Clone owner failed".to_string(),
                            )),
                        ));
                    }
                    Err(_) => {
                        return Err(ClonerError::FailedToCloneRegularAccount(
                            pubkey,
                            Box::new(ClonerError::CommittorServiceError(
                                "Clone owner dropped".to_string(),
                            )),
                        ));
                    }
                },
            }
        }
    }

    fn claim_undelegation(
        &self,
        pubkey: Pubkey,
    ) -> Option<PendingUndelegationGuard> {
        let mut pending_undelegations =
            self.pending_undelegations.lock().ok()?;
        if !pending_undelegations.insert(pubkey) {
            return None;
        }
        Some(PendingUndelegationGuard {
            pending_undelegations: Arc::clone(&self.pending_undelegations),
            pubkey,
        })
    }

    fn normalize_unresolved_dlp_clone_request(
        &self,
        request: &mut AccountCloneRequest,
    ) -> ChainlinkResult<()> {
        // Both modes are claims that this validator owns the account: confined
        // accounts used to carry the delegated flag as well, so a single
        // `delegated()` check covered them. With exclusive modes they have to be
        // named separately, or a stale confinement would never be normalized.
        let claims_delegation = request.account.is(AccountMode::Delegated)
            || request.account.is(AccountMode::Ephemeral);
        if request.account.owner() != &dlp_api::id() || !claims_delegation {
            return Ok(());
        }

        if request.pubkey
            == dlp_api::pda::magic_fee_vault_pda_from_validator(
                &self.validator_pubkey,
            )
        {
            return Ok(());
        }

        if !request.delegation_actions.is_empty() {
            return Err(ChainlinkError::InvalidDelegationActions(
                request.pubkey,
                "post-delegation actions attached to unresolved DLP-owned clone target"
                    .to_string(),
            ));
        }

        // Neither delegated nor confined: a single readonly mode covers both.
        request.account.set_mode(AccountMode::ReadOnly);
        Ok(())
    }

    fn local_delegated_clone_target_active(&self, pubkey: Pubkey) -> bool {
        self.get_account(&pubkey)
            .is_some_and(|account| account.is(AccountMode::Delegated))
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
        if self
            .maybe_greedily_clone_discovered_delegated_account(pubkey, &update)
            .await
        {
            return;
        }

        if matches!(update.source, SubscriptionSource::Program)
            && update.account.is_owned_by_delegation_program()
            && update.account.fresh_account().is_some_and(|account| {
                is_internal_dlp_account_data(account.data())
            })
        {
            trace!(
                pubkey = %pubkey,
                "Dropping internal DLP program subscription update after discovery miss"
            );
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
            if self.get_account(&pubkey).is_some()
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
            .maybe_build_projected_ata_clone_request_from_subscription_update(
                pubkey,
                &account,
                deleg_record.as_ref(),
                &delegation_actions,
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
        let non_advancing_slot =
            self.get_account(&pubkey).and_then(|in_bank| {
                let bank_slot = in_bank.slot();
                let update_slot = account.slot();
                let same_slot_delegated_refresh = bank_slot == update_slot
                    && account.is(AccountMode::Delegated)
                    && (!in_bank.is(AccountMode::Delegated)
                        || in_bank.is(AccountMode::Transient));
                if bank_slot > update_slot
                    || (bank_slot == update_slot
                        && !same_slot_delegated_refresh)
                {
                    Some(bank_slot)
                } else {
                    None
                }
            });

        if let Some(in_bank_slot) = non_advancing_slot {
            let update_slot = account.slot();
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
        if let Some(in_bank) = self.get_account(&pubkey) {
            if in_bank.is(AccountMode::Delegated)
                && !in_bank.is(AccountMode::Transient)
            {
                self.cleanup_direct_subscription_for_delegated_account(pubkey)
                    .await;
                return;
            }

            if in_bank.is(AccountMode::Transient) {
                debug!(
                    pubkey = %pubkey,
                    in_bank_delegated = in_bank.is(AccountMode::Delegated),
                    in_bank_owner = %in_bank.owner(),
                    in_bank_slot = in_bank.slot(),
                    chain_delegated = account.is(AccountMode::Delegated),
                    chain_owner = %account.owner(),
                    chain_slot = account.slot(),
                    "Received update for undelegating account"
                );

                if account.is(AccountMode::Delegated)
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
                    account.is(AccountMode::Delegated),
                    in_bank.slot(),
                    deleg_record,
                    &self.validator_pubkey,
                ) {
                    return;
                }
                undelegation_completed_on_chain = true;
            } else if !in_bank.is(AccountMode::Delegated)
                && account.is(AccountMode::Delegated)
            {
                undelegation_completed_on_chain = true;
            } else if in_bank.owner().eq(&dlp_api::id()) {
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
            if account.is(AccountMode::Delegated) {
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
            if !account.is(AccountMode::Delegated) {
                self.ensure_direct_subscription_for_completed_account(pubkey)
                    .await;
            }
            self.cleanup_undelegation_tracking_for_completed_account(pubkey)
                .await;
        }
        if account.is(AccountMode::Delegated) {
            self.cleanup_direct_subscription_for_delegated_account(pubkey)
                .await;
        }

        if account.executable() {
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
            let raw_delegation_actions = if account.is(AccountMode::Delegated)
                && projected_ata_clone_request.is_none()
            {
                delegation_actions
            } else {
                DelegationActions::default()
            };
            if let Err(err) = self
                .clone_account_with_post_delegation_action_invariants(
                    AccountCloneRequest {
                        pubkey,
                        account,
                        commit_frequency_ms,
                        delegation_actions: raw_delegation_actions,
                        delegated_to_other,
                        needs_undelegation: false,
                    },
                    subscription_clone_context,
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
        delegation_actions: &'a DelegationActions,
        fetch_context: AccountFetchContext,
    ) -> Pin<Box<dyn Future<Output = ChainlinkResult<()>> + Send + 'a>> {
        Box::pin(async move {
            if delegation_actions.is_empty() {
                return Ok(());
            }

            self.validate_post_delegation_action_signers(delegation_actions)
                .await?;

            let (dependencies, writable_dependencies) =
                Self::collect_post_delegation_action_dependencies(
                    pubkey,
                    delegation_actions,
                );

            let dependencies_to_fetch = {
                let accessor = self.engine.accounts();
                let loader = accessor.loader();
                dependencies
                    .into_iter()
                    .filter(|dependency| {
                        let Some(account) =
                            loader.load(dependency).ok().flatten()
                        else {
                            return true;
                        };
                        writable_dependencies.contains(dependency)
                            && (!account.is(AccountMode::Delegated)
                                || account.is(AccountMode::Transient))
                    })
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
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
                )
                .await?;
            if result.missing_delegation_record.is_empty() {
                return Ok(());
            }

            let missing_accounts = result
                .pubkeys_missing_delegation_record()
                .into_iter()
                .collect::<HashSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let mut missing_accounts = missing_accounts;
            missing_accounts.sort_unstable();
            Err(ChainlinkError::MissingDelegationActionAccounts(
                missing_accounts,
            ))
        })
    }

    fn collect_post_delegation_action_dependencies(
        target: Pubkey,
        delegation_actions: &DelegationActions,
    ) -> (HashSet<Pubkey>, HashSet<Pubkey>) {
        let mut dependencies = HashSet::new();
        let mut writable_dependencies = HashSet::new();
        for instruction in delegation_actions.iter() {
            if instruction.program_id != target {
                dependencies.insert(instruction.program_id);
            }
            for meta in &instruction.accounts {
                if meta.pubkey == target {
                    continue;
                }
                dependencies.insert(meta.pubkey);
                if meta.is_writable {
                    writable_dependencies.insert(meta.pubkey);
                }
            }
        }
        (dependencies, writable_dependencies)
    }

    async fn validate_post_delegation_action_signers(
        &self,
        delegation_actions: &DelegationActions,
    ) -> ChainlinkResult<()> {
        let Some(risk_service) = self.risk_service.as_ref() else {
            return Ok(());
        };

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
        if self
            .get_account(&request.pubkey)
            .is_some_and(|account| account.is(AccountMode::Transient))
        {
            return Ok(());
        }

        self.clone_account_with_post_delegation_action_invariants(
            request,
            fetch_context.with_reason(AccountFetchReason::AtaProjection),
        )
        .await
    }

    async fn maybe_greedily_clone_discovered_delegated_account(
        &self,
        pubkey: Pubkey,
        update: &ForwardedSubscriptionUpdate,
    ) -> bool {
        if self.get_account(&pubkey).is_some() {
            return false;
        }

        let Some(account) = update.account.fresh_account() else {
            return false;
        };

        if !account.owner().eq(&dlp_api::id()) {
            return false;
        }

        let discovery_context = AccountFetchContext::subscription_update(
            AccountFetchReason::SubscriptionUpdateGreedyDiscovery,
        );
        let record_context =
            discovery_context.with_reason(AccountFetchReason::DelegationRecord);

        let Some((deleg_record, delegation_actions)) = self
            .fetch_and_parse_delegation_record(
                pubkey,
                account.slot(),
                record_context,
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
        let delegation_actions = delegation_actions.unwrap_or_default();

        let greedy_ata_pubkeys = delegation::parse_raw_eata_pda(
            &pubkey,
            account.data(),
            deleg_record.owner,
        )
        .map(|(wallet_owner, mint)| {
            try_derive_supported_ata_pubkeys(&wallet_owner, &mint)
                .token_2022_first()
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
        let mut pubkeys_to_clone =
            Vec::with_capacity(1 + greedy_ata_pubkeys.len());
        pubkeys_to_clone.push(pubkey);
        {
            let accessor = self.engine.accounts();
            let loader = accessor.loader();
            pubkeys_to_clone.extend(
                greedy_ata_pubkeys.iter().copied().filter(|ata_pubkey| {
                    loader.load(ata_pubkey).ok().flatten().is_none()
                }),
            );
        }

        // Keep eATA discovery with its candidate base ATAs in one clone batch
        // so the normal ATA projection path runs for the same update.
        let clone_result = if greedy_ata_pubkeys.is_empty() {
            self.fetch_and_clone_accounts_with_dedup(
                &pubkeys_to_clone,
                None,
                Some(account.slot()),
                discovery_context,
            )
            .await
        } else {
            self.fetch_and_clone_accounts(
                &pubkeys_to_clone,
                None,
                Some(account.slot()),
                discovery_context,
            )
            .await
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
                    self.get_account(&pubkey).map(|in_bank| in_bank.slot());
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
                    .maybe_build_projected_ata_clone_request_from_subscription_update(
                        pubkey,
                        &account,
                        Some(&deleg_record),
                        &delegation_actions,
                        &CompanionFetchLogContext {
                            origin: discovery_context,
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
                            discovery_context,
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
                                .load(ata_pubkey)
                                .ok()
                                .flatten()
                                .is_some_and(|account_in_bank| {
                                    account_in_bank.slot() >= account.slot()
                                })
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
        account: AccountSharedData,
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
        Option<AccountSharedData>,
        Option<DelegationRecord>,
        DelegationActions,
    ) {
        let ForwardedSubscriptionUpdate {
            pubkey,
            account,
            source: _,
        } = update;
        let owned_by_delegation_program =
            account.is_owned_by_delegation_program();

        if let Some(account) = account.fresh_account() {
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
                        mut account,
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
                                    delegation_record.data(),
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
                                        slot = account.remote_slot(),
                                        owner = %delegation_record.owner,
                                        deleg_record = %delegation_record_display,
                                        "Resolving delegated account"
                                    );
                                }

                                self.apply_delegation_record_to_account(
                                    pubkey,
                                    &mut account,
                                    &delegation_record,
                                );

                                // For accounts delegated to us, subscribe to the original owner
                                // program for undelegation update resilience.
                                if account.delegated()
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
                                    Some(account.into_account_shared_data()),
                                    Some(delegation_record),
                                    delegation_actions.unwrap_or_default(),
                                )
                            } else {
                                // If the delegation record is invalid we cannot clone the account
                                // since something is corrupt and we wouldn't know what owner to
                                // use, etc.
                                (None, None, DelegationActions::default())
                            }
                        } else if let Ok(request) =
                            UndelegationRequest::try_from_bytes_with_discriminator(
                                account.data(),
                            )
                        {
                            let observed = ObservedUndelegationRequest {
                                request_pda: pubkey,
                                delegated_account: request.delegated_account,
                                expires_at_slot: request.expires_at_slot,
                                observed_slot: account.remote_slot(),
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
                                Some(account.into_account_shared_data()),
                                None,
                                DelegationActions::default(),
                            )
                        } else if is_internal_dlp_account_data(account.data()) {
                            (
                                Some(account.into_account_shared_data()),
                                None,
                                DelegationActions::default(),
                            )
                        } else {
                            trace!(
                                pubkey = %pubkey,
                                "Skipping DLP-owned subscription update without delegation record"
                            );
                            (None, None, DelegationActions::default())
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
                        (None, None, DelegationActions::default())
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
                        (None, None, DelegationActions::default())
                    }
                }
            } else {
                let (account, deleg_record) = self
                    .maybe_project_ata_from_subscription_update(
                        pubkey,
                        account,
                        companion_fetch_log_context,
                    )
                    .await;
                if let Some((deleg_record, actions)) = deleg_record {
                    (
                        Some(account),
                        Some(deleg_record),
                        actions.unwrap_or_default(),
                    )
                } else {
                    (Some(account), None, DelegationActions::default())
                }
            }
        } else {
            // This should not happen since we call this method with sub updates which always hold
            // a fresh remote account
            error!(pubkey = %pubkey, account = ?account, "BUG: Received subscription update without fresh account");
            (None, None, DelegationActions::default())
        }
    }

    async fn maybe_build_projected_ata_clone_request_from_subscription_update(
        &self,
        eata_pubkey: Pubkey,
        eata_account: &AccountSharedData,
        deleg_record: Option<&DelegationRecord>,
        delegation_actions: &DelegationActions,
        companion_fetch_log_context: &CompanionFetchLogContext,
    ) -> Option<AccountCloneRequest> {
        ata_projection::maybe_build_projected_ata_clone_request_from_subscription_update(
            self,
            eata_pubkey,
            eata_account,
            deleg_record,
            delegation_actions,
            companion_fetch_log_context,
        )
        .await
    }

    async fn maybe_project_ata_from_subscription_update(
        &self,
        ata_pubkey: Pubkey,
        ata_account: AccountSharedData,
        companion_fetch_log_context: &CompanionFetchLogContext,
    ) -> (
        AccountSharedData,
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
        account: &mut ResolvedAccountSharedData,
        delegation_record: &DelegationRecord,
    ) -> Option<u64> {
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

    /// Tries to fetch all accounts in `pubkeys` and clone them into the bank.
    /// If `mark_empty` is provided, accounts in that list that are
    /// not found on chain will be added with zero lamports to the bank.
    ///
    /// - **pubkeys**: list of accounts to fetch and clone
    /// - **mark_empty**: optional list of accounts that should be added as empty if not found on
    ///   chain
    /// - **slot**: optional slot to use as minimum context slot for the accounts being cloned
    ///
    /// NOTE: accounts fetched here have not been found in the bank
    async fn fetch_and_clone_accounts(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        let accs = match self
            .fetch_accounts(
                pubkeys,
                mark_empty_if_not_found,
                slot,
                fetch_context,
            )
            .await
        {
            Ok(accs) => accs,
            Err(err) => {
                for _ in pubkeys {
                    metrics::inc_chainlink_clone_accounts_total_with_context(
                        fetch_context,
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

    #[instrument(skip(self, pubkeys, mark_empty_if_not_found), fields(tx_sig = tracing::field::Empty))]
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

    #[instrument(skip(self, pubkeys, accs, mark_empty_if_not_found), fields(tx_sig = tracing::field::Empty))]
    async fn clone_accounts(
        &self,
        pubkeys: &[Pubkey],
        accs: Vec<RemoteAccount>,
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<FetchAndCloneResult> {
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

        // For accounts we couldn't find we cannot do anything. We will let code depending
        // on them to be in the bank fail on its own
        if !not_found.is_empty() {
            trace!(
                "Could not find accounts on chain: {:?}",
                not_found
                    .iter()
                    .map(|(pubkey, slot)| (pubkey.to_string(), *slot))
                    .collect::<Vec<_>>()
            );
        }

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
            fetch_context,
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
            fetch_context,
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
            fetch_context,
        )
        .await;
        accounts_to_clone.extend(ata_accounts);

        // Ensure all accounts referenced by delegation actions exist and are
        // cloned before we execute those actions as part of account cloning.
        let action_dependencies =
            pipeline::collect_delegation_action_dependencies(
                &accounts_to_clone,
            );
        let action_dependencies_to_fetch = {
            let accessor = self.engine.accounts();
            let loader = accessor.loader();
            action_dependencies
                .into_iter()
                .filter(|dependency| {
                    loader.load(dependency).ok().flatten().is_none()
                        && !accounts_to_clone
                            .iter()
                            .any(|request| request.pubkey.eq(dependency))
                        && !loaded_programs
                            .iter()
                            .any(|program| program.program_id.eq(dependency))
                })
                .collect::<Vec<_>>()
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
                .with_reason(AccountFetchReason::ActionDependencyMissing);
            let action_dep_accs = self
                .remote_account_provider
                .try_get_multi(
                    &action_dependencies_to_fetch,
                    None,
                    action_dependency_context,
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
                action_dependency_context,
            )
            .await
            {
                Ok(resolved) => resolved,
                Err(err) => {
                    let releases = pipeline::compute_subscription_releases(
                        &all_requested_pubkeys,
                        &accounts_to_clone,
                        &loaded_programs,
                        record_subs.clone(),
                        program_data_subs.clone(),
                    );
                    release_subs(&self.remote_account_provider, releases).await;
                    return Err(err);
                }
            };

            if !action_dep_missing_delegation_record.is_empty() {
                let releases = pipeline::compute_subscription_releases(
                    &all_requested_pubkeys,
                    &accounts_to_clone,
                    &loaded_programs,
                    record_subs
                        .iter()
                        .copied()
                        .chain(action_dep_record_subs.iter().copied())
                        .collect(),
                    program_data_subs.clone(),
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
                action_dependency_context,
            )
            .await
            {
                Ok(resolved) => resolved,
                Err(err) => {
                    let mut cleanup_accounts_to_clone =
                        accounts_to_clone.clone();
                    cleanup_accounts_to_clone
                        .extend(action_dep_accounts_to_clone.clone());
                    let releases = pipeline::compute_subscription_releases(
                        &all_requested_pubkeys,
                        &cleanup_accounts_to_clone,
                        &loaded_programs,
                        record_subs.clone(),
                        program_data_subs.clone(),
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
            &accounts_to_clone,
            &loaded_programs,
            record_subs,
            program_data_subs,
        );

        pipeline::clone_accounts_and_programs(
            self,
            accounts_to_clone,
            loaded_programs,
            fetch_context,
        )
        .await?;

        release_subs(&self.remote_account_provider, releases).await;

        Ok(FetchAndCloneResult {
            not_found_on_chain: not_found,
            missing_delegation_record,
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
        in_bank: &AccountSharedData,
        fetch_context: AccountFetchContext,
    ) -> RefreshDecision {
        if in_bank.is(AccountMode::Transient) {
            debug!(
                pubkey = %pubkey,
                delegated = in_bank.is(AccountMode::Delegated),
                undelegating = in_bank.is(AccountMode::Transient),
                "Fetching undelegating account"
            );

            if let Some(eata_pubkey) =
                ata_projection::derive_eata_pubkey_from_ata_layout(
                    pubkey, in_bank,
                )
            {
                let undelegating_refresh_context = fetch_context
                    .with_reason(AccountFetchReason::UndelegatingRefresh);
                let projected_deleg_record = self
                    .fetch_and_parse_delegation_record(
                        eata_pubkey,
                        self.remote_account_provider.chain_slot(),
                        undelegating_refresh_context,
                        CompanionFetchLogContext {
                            origin: undelegating_refresh_context,
                            primary_pubkey: eata_pubkey,
                            context_slot: self
                                .remote_account_provider
                                .chain_slot(),
                        },
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
                .with_reason(AccountFetchReason::UndelegatingRefresh);
            let deleg_record = self
                .fetch_and_parse_delegation_record(
                    *pubkey,
                    self.remote_account_provider.chain_slot(),
                    undelegating_refresh_context,
                    CompanionFetchLogContext {
                        origin: undelegating_refresh_context,
                        primary_pubkey: *pubkey,
                        context_slot: self.remote_account_provider.chain_slot(),
                    },
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
                in_bank.slot(),
                deleg_record,
                &self.validator_pubkey,
            ) {
                debug!(
                    "Account {pubkey} marked as undelegating will be overridden since undelegation completed"
                );
                return RefreshDecision::Yes;
            }
        } else if in_bank.owner().eq(&dlp_api::id()) {
            debug!(
                "Account {pubkey} owned by deleg program not marked as undelegating"
            );
        }
        RefreshDecision::No
    }

    /// Fetches and clones accounts while the engine coordinates concurrent
    /// requests for the same missing account.
    #[instrument(skip(self, pubkeys, mark_empty_if_not_found))]
    pub async fn fetch_and_clone_accounts_with_dedup(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
        slot: Option<u64>,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        self.fetch_and_clone_accounts_with_dedup_forced_refresh(
            pubkeys,
            mark_empty_if_not_found,
            slot,
            fetch_context,
            &HashSet::new(),
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
    ) -> ChainlinkResult<FetchAndCloneResult> {
        let mut pubkeys = pubkeys.iter().collect::<Vec<_>>();
        if tracing::enabled!(tracing::Level::TRACE) {
            let count = pubkeys.len();
            trace!(count, "Fetching and cloning accounts with dedup");
        }

        let requested_pubkeys =
            pubkeys.iter().map(|pubkey| **pubkey).collect::<Vec<_>>();
        let mut loads = Vec::new();
        let mut load_pubkeys = HashSet::new();
        let mut waiters = Vec::new();
        for missing in self.engine.accounts().ensure(&requested_pubkeys) {
            match missing {
                MissingAccount::Load(load) => {
                    load_pubkeys.insert(load.pubkey);
                    loads.push(load);
                }
                MissingAccount::Wait(wait) => waiters.push(wait),
            }
        }

        let mut in_bank = HashSet::new();
        let mut refresh = HashSet::new();
        let mut extra_mark_empty = vec![];
        let mut bank_hit_no_fetch_non_undelegating_count = 0_u64;
        let mut bank_hit_no_fetch_undelegating_still_valid_count = 0_u64;
        let mut bank_hit_no_fetch_undelegating_timeout_count = 0_u64;
        let mut bank_hit_undelegating_refresh_required_count = 0_u64;
        let mut bank_miss_remote_required_count = 0_u64;
        let mut forced_refresh_remote_required_count = 0_u64;

        // Phase 1: Sync bank check — separate undelegating accounts
        // (which need async RPC) from non-undelegating (handled
        // synchronously)
        let mut undelegating_checks: Vec<(Pubkey, AccountSharedData)> = vec![];
        {
            let accessor = self.engine.accounts();
            let loader = accessor.loader();
            for pubkey in pubkeys.iter() {
                if force_refresh_pubkeys.contains(*pubkey) {
                    forced_refresh_remote_required_count += 1;
                    refresh.insert(**pubkey);
                    continue;
                }
                if let Some(account_in_bank) = loader.load(pubkey).ok().flatten() {
                    if account_in_bank.is(AccountMode::Transient) {
                        undelegating_checks.push((**pubkey, account_in_bank));
                    } else {
                        if account_in_bank.owner().eq(&dlp_api::id()) {
                            debug!(
                                pubkey = %pubkey,
                                "Account owned by deleg program not marked as undelegating"
                            );
                        }
                        if tracing::enabled!(tracing::Level::TRACE) {
                            let delegated =
                                account_in_bank.is(AccountMode::Delegated);
                            let owner = account_in_bank.owner();
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
                } else {
                    bank_miss_remote_required_count += 1;
                }
            }
        }

        // Phase 2: Parallel undelegation checks via JoinSet
        if !undelegating_checks.is_empty() {
            let mut join_set = JoinSet::new();
            for (pubkey, account_in_bank) in undelegating_checks {
                let this = self.clone();
                join_set.spawn(async move {
                    let decision = match tokio::time::timeout(
                        Duration::from_secs(5),
                        this.should_refresh_undelegating_in_bank_account(
                            &pubkey,
                            &account_in_bank,
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
                        refresh.insert(pubkey);
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
            fetch_context,
            BankPrecheckOutcome::BankHitNoFetch,
            BankPrecheckReason::NonUndelegatingPresent,
            bank_hit_no_fetch_non_undelegating_count,
        );
        metrics::inc_chainlink_bank_precheck_accounts_with_context(
            fetch_context,
            BankPrecheckOutcome::BankHitNoFetch,
            BankPrecheckReason::UndelegatingStillValid,
            bank_hit_no_fetch_undelegating_still_valid_count,
        );
        metrics::inc_chainlink_bank_precheck_accounts_with_context(
            fetch_context,
            BankPrecheckOutcome::BankHitNoFetch,
            BankPrecheckReason::UndelegatingCheckTimeout,
            bank_hit_no_fetch_undelegating_timeout_count,
        );
        metrics::inc_chainlink_bank_precheck_accounts_with_context(
            fetch_context.with_reason(AccountFetchReason::UndelegatingRefresh),
            BankPrecheckOutcome::BankHitUndelegatingRefreshRequired,
            BankPrecheckReason::UndelegatingRefresh,
            bank_hit_undelegating_refresh_required_count,
        );
        metrics::inc_chainlink_bank_precheck_accounts_with_context(
            fetch_context,
            BankPrecheckOutcome::BankMissRemoteRequired,
            BankPrecheckReason::Absent,
            bank_miss_remote_required_count,
        );
        metrics::inc_chainlink_bank_precheck_accounts_with_context(
            fetch_context,
            BankPrecheckOutcome::ForcedRefreshRemoteRequired,
            BankPrecheckReason::ForcedRefresh,
            forced_refresh_remote_required_count,
        );
        pubkeys.retain(|p| !in_bank.contains(p));

        let mut mark_empty_set = mark_empty_if_not_found
            .unwrap_or(&[])
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        mark_empty_set.extend(extra_mark_empty);
        let mark_empty = mark_empty_set.iter().copied().collect::<Vec<_>>();
        let mark_empty =
            (!mark_empty.is_empty()).then_some(mark_empty.as_slice());

        // Existing accounts that require a forced lifecycle refresh are not
        // returned by `ensure`; missing accounts are fetched only by the caller
        // holding the engine load reservation.
        let fetch_pubkeys = pubkeys
            .into_iter()
            .copied()
            .filter(|pubkey| {
                refresh.contains(pubkey) || load_pubkeys.contains(pubkey)
            })
            .collect::<Vec<_>>();
        let final_result = if fetch_pubkeys.is_empty() {
            FetchAndCloneResult::default()
        } else {
            self.fetch_and_clone_accounts(
                &fetch_pubkeys,
                mark_empty,
                slot,
                fetch_context,
            )
            .await?
        };

        let completed_loads = {
            let accessor = self.engine.accounts();
            let loader = accessor.loader();
            loads
                .into_iter()
                .map(|load| {
                    let track = loader
                        .load(&load.pubkey)
                        .ok()
                        .flatten()
                        .is_some_and(|account| !account.mutable());
                    (load, track)
                })
                .collect::<Vec<_>>()
        };
        let mut tracked_since_yield = 0;
        for (load, track) in completed_loads {
            load.complete(track);
            if track {
                tracked_since_yield += 1;
                if tracked_since_yield == 16 {
                    // Engine eviction notifications use a small broadcast
                    // buffer. Let Chainlink's listener drain between chunks
                    // when one batched fetch completes many tracked loads.
                    task::yield_now().await;
                    tracked_since_yield = 0;
                }
            }
        }

        for waiter in waiters {
            let (pubkey, completed) = waiter.wait().await;
            if !completed {
                return Err(ChainlinkError::AccountLoadFailed(pubkey));
            }
        }

        Ok(final_result)
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
            ResolvedAccount::Fresh(account) => {
                Some(ResolvedAccountSharedData::Fresh(account.clone()))
            }
            ResolvedAccount::Bank((pubkey, _)) => loader
                .load(pubkey)
                .ok()
                .flatten()
                .map(ResolvedAccountSharedData::Bank),
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
                let Some(comp_account) =
                    resolve(&comp.account)
                else {
                    return Err(
                        ChainlinkError::ResolvedCompanionAccountCouldNoLongerBeFound(
                            companion_pubkey,
                        ),
                    );
                };
                let Some(account) =
                    resolve(&acc.account)
                else {
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

    /// Best-effort airdrop helper: if the account doesn't exist in the bank or has 0 lamports,
    /// create/overwrite it as a plain system account with the provided lamports using the cloner path.
    #[instrument(skip(self))]
    pub async fn airdrop_account_if_empty(
        &self,
        pubkey: Pubkey,
        lamports: u64,
    ) -> ClonerResult<()> {
        if lamports == 0 {
            return Ok(());
        }
        let remote_slot = if let Some(acc) = self.get_account(&pubkey) {
            if acc.lamports() > 0 {
                return Ok(());
            }
            acc.slot().max(self.remote_account_provider.chain_slot())
        } else {
            self.remote_account_provider.chain_slot()
        };
        // Build a plain system account with the requested balance
        let mut account =
            AccountSharedData::new(lamports, 0, &system_program::id());
        AccountFieldPatch::Slot(remote_slot).apply(&mut account);
        debug!(
            pubkey = %pubkey,
            lamports,
            remote_slot,
            "Auto-airdropping account"
        );
        cloner::clone_account(
            &self.engine,
            AccountCloneRequest {
                pubkey,
                account,
                commit_frequency_ms: None,
                delegation_actions: DelegationActions::default(),
                delegated_to_other: None,
                needs_undelegation: false,
            },
        )
        .await?;
        Ok(())
    }
}
