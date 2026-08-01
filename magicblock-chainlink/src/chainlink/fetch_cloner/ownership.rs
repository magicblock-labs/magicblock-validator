//! Clone flows that resolve and apply chain ownership.

use std::sync::Arc;

use magicblock_accounts_db::traits::AccountsBank;
use magicblock_core::token_programs::{
    is_ata, normalize_native_token_account_for_local_clone,
};
use magicblock_metrics::metrics::{
    self, AccountFetchContext, ChainlinkCloneIntent,
    ChainlinkCloneMaterializationOutcome, ChainlinkCloneOutcome,
    ChainlinkCloneRemoteResult, ChainlinkEmptyPlaceholderStage, Outcome,
};
use solana_account::{AccountSharedData, ReadableAccount};
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use tracing::*;

use super::*;

impl<T, U, V, C> FetchCloner<T, U, V, C>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    pub(super) fn account_is_actively_delegated(
        account: &AccountSharedData,
    ) -> bool {
        account.delegated() && !account.undelegating()
    }

    pub(super) fn local_account_satisfies_clone_request(
        &self,
        request: &AccountCloneRequest,
    ) -> bool {
        let active_delegation_satisfies_request =
            request.delegation_actions.is_empty()
                && request.delegated_to_other.is_none()
                && !request.needs_undelegation;
        self.accounts_bank
            .get_account(&request.pubkey)
            .is_some_and(|account| {
                let local_slot = account.remote_slot();
                let request_slot = request.account.remote_slot();
                (active_delegation_satisfies_request
                    && Self::account_is_actively_delegated(&account))
                    || local_slot > request_slot
                    || (local_slot == request_slot
                        && account.eq(&request.account))
            })
    }

    pub(super) fn is_empty_placeholder_account(
        account: &AccountSharedData,
    ) -> bool {
        account.lamports() == 0
            && account.data().is_empty()
            && account.owner() == &Pubkey::default()
            && !account.executable()
    }

    pub(super) fn clone_remote_result_for_request(
        request: &AccountCloneRequest,
    ) -> ChainlinkCloneRemoteResult {
        if Self::is_empty_placeholder_account(&request.account) {
            ChainlinkCloneRemoteResult::NotFound
        } else {
            ChainlinkCloneRemoteResult::Found
        }
    }

    pub(super) fn clone_intent_for_request(
        request: &AccountCloneRequest,
    ) -> ChainlinkCloneIntent {
        if Self::is_empty_placeholder_account(&request.account) {
            ChainlinkCloneIntent::EmptyPlaceholder
        } else if request.account.delegated() {
            ChainlinkCloneIntent::DelegationRecord
        } else if !request.delegation_actions.is_empty() {
            ChainlinkCloneIntent::ActionDependency
        } else {
            ChainlinkCloneIntent::NormalAccount
        }
    }

    pub(super) fn record_account_materialization(
        &self,
        pubkey: &Pubkey,
        request_slot: u64,
        remote_result: ChainlinkCloneRemoteResult,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkCloneMaterializationOutcome {
        let outcome = if self
            .accounts_bank
            .get_account(pubkey)
            .is_some_and(|account| account.remote_slot() >= request_slot)
        {
            ChainlinkCloneMaterializationOutcome::ObservedInBankAfterEnsure
        } else {
            ChainlinkCloneMaterializationOutcome::StillMissingAfterEnsure
        };
        metrics::inc_chainlink_clone_materialization_accounts_total_with_context(
            fetch_context,
            remote_result,
            outcome,
        );
        outcome
    }

    pub(super) fn record_empty_placeholder_stage(
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

    pub(super) fn record_empty_placeholder_materialization_stage(
        is_empty_placeholder: bool,
        fetch_context: AccountFetchContext,
        materialization_outcome: ChainlinkCloneMaterializationOutcome,
    ) {
        let (stage, outcome) = match materialization_outcome {
            ChainlinkCloneMaterializationOutcome::ObservedInBankAfterEnsure => (
                ChainlinkEmptyPlaceholderStage::ObservedInBankAfterEnsure,
                Outcome::Success,
            ),
            ChainlinkCloneMaterializationOutcome::StillMissingAfterEnsure
            | ChainlinkCloneMaterializationOutcome::RemovedAfterMaterialization => (
                ChainlinkEmptyPlaceholderStage::StillMissingAfterEnsure,
                Outcome::Error,
            ),
        };
        Self::record_empty_placeholder_stage(
            is_empty_placeholder,
            fetch_context,
            stage,
            outcome,
        );
    }

    pub(super) fn record_program_materialization(
        &self,
        program_id: &Pubkey,
        remote_slot: u64,
        fetch_context: AccountFetchContext,
    ) {
        let outcome = if self
            .accounts_bank
            .get_account(program_id)
            .is_some_and(|account| account.remote_slot() >= remote_slot)
        {
            ChainlinkCloneMaterializationOutcome::ObservedInBankAfterEnsure
        } else {
            ChainlinkCloneMaterializationOutcome::StillMissingAfterEnsure
        };
        metrics::inc_chainlink_clone_materialization_accounts_total_with_context(
            fetch_context,
            ChainlinkCloneRemoteResult::Found,
            outcome,
        );
    }

    /// Submits a clone request through ownership coordination.
    /// Only one account clone per pubkey is submitted at a time. Waiters
    /// retry local freshness checks after a successful owner so a newer
    /// request can clone after an older request finishes.
    pub(super) async fn clone_account_with_ownership(
        &self,
        request: AccountCloneRequest,
        fetch_context: AccountFetchContext,
    ) -> ClonerResult<Signature> {
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
                    fetch_context.clone(),
                    remote_result,
                    clone_intent,
                    ChainlinkCloneOutcome::Skipped,
                );
                return Ok(Signature::default());
            }

            match self.claim_pending_clone(pubkey) {
                CloneClaim::Owner => {
                    let mut guard = PendingCloneGuard::new(
                        Arc::clone(&self.pending_clones),
                        pubkey,
                    );
                    metrics::inc_chainlink_clone_accounts_total_with_context(
                        fetch_context.clone(),
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
                    let request_slot = owned_request.account.remote_slot();
                    Self::record_empty_placeholder_stage(
                        is_empty_placeholder,
                        fetch_context.clone(),
                        ChainlinkEmptyPlaceholderStage::CloneSubmitted,
                        Outcome::Success,
                    );
                    let result = self.cloner.clone_account(owned_request).await;
                    let reconciled_active_delegation = result.is_err()
                        && active_delegation_satisfies_request
                        && self.accounts_bank.get_account(&pubkey).is_some_and(
                            |account| {
                                Self::account_is_actively_delegated(&account)
                            },
                        );
                    if reconciled_active_delegation {
                        debug!(
                            pubkey = %pubkey,
                            error = ?result,
                            "Clone request satisfied by concurrently active local delegation"
                        );
                        metrics::inc_chainlink_clone_accounts_total_with_context(
                            fetch_context.clone(),
                            remote_result,
                            clone_intent,
                            ChainlinkCloneOutcome::Skipped,
                        );
                    } else if result.is_ok() {
                        metrics::inc_chainlink_clone_accounts_total_with_context(
                            fetch_context.clone(),
                            remote_result,
                            clone_intent,
                            ChainlinkCloneOutcome::CloneSucceeded,
                        );
                        let materialization_outcome = self
                            .record_account_materialization(
                                &pubkey,
                                request_slot,
                                remote_result,
                                fetch_context.clone(),
                            );
                        Self::record_empty_placeholder_materialization_stage(
                            is_empty_placeholder,
                            fetch_context.clone(),
                            materialization_outcome,
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
                            fetch_context.clone(),
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
                        Ok(Signature::default())
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

    pub(super) async fn clone_program_with_ownership(
        &self,
        program: LoadedProgram,
        fetch_context: AccountFetchContext,
    ) -> ClonerResult<Signature> {
        let program_id = program.program_id;
        let remote_slot = program.remote_slot;
        let remote_result = ChainlinkCloneRemoteResult::Found;
        let clone_intent = ChainlinkCloneIntent::ProgramData;

        loop {
            if self
                .accounts_bank
                .get_account(&program_id)
                .is_some_and(|account| account.remote_slot() >= remote_slot)
            {
                metrics::inc_chainlink_clone_accounts_total_with_context(
                    fetch_context.clone(),
                    remote_result,
                    clone_intent,
                    ChainlinkCloneOutcome::Skipped,
                );
                return Ok(Signature::default());
            }

            match self.claim_pending_clone(program_id) {
                CloneClaim::Owner => {
                    let mut guard = PendingCloneGuard::new(
                        Arc::clone(&self.pending_clones),
                        program_id,
                    );

                    let result = if self
                        .accounts_bank
                        .get_account(&program_id)
                        .is_some_and(|account| {
                            account.remote_slot() >= remote_slot
                        }) {
                        metrics::inc_chainlink_clone_accounts_total_with_context(
                            fetch_context.clone(),
                            remote_result,
                            clone_intent,
                            ChainlinkCloneOutcome::Skipped,
                        );
                        Ok(Signature::default())
                    } else {
                        metrics::inc_chainlink_clone_accounts_total_with_context(
                            fetch_context.clone(),
                            remote_result,
                            clone_intent,
                            ChainlinkCloneOutcome::Submitted,
                        );
                        let result = self.cloner.clone_program(program).await;
                        if result.is_ok() {
                            metrics::inc_chainlink_clone_accounts_total_with_context(
                                fetch_context.clone(),
                                remote_result,
                                clone_intent,
                                ChainlinkCloneOutcome::CloneSucceeded,
                            );
                            self.record_program_materialization(
                                &program_id,
                                remote_slot,
                                fetch_context,
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

    pub(super) async fn clone_account_with_post_delegation_action_invariants(
        &self,
        mut request: AccountCloneRequest,
        fetch_context: AccountFetchContext,
    ) -> ChainlinkResult<Signature> {
        if request.account.delegated()
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

        if request.account.delegated()
            && self.local_delegated_clone_target_active(request.pubkey)
        {
            return Ok(Signature::default());
        }

        if request.delegation_actions.is_empty() {
            return Ok(self
                .clone_account_with_ownership(request, fetch_context)
                .await?);
        }

        if !request.account.delegated() {
            return Err(ChainlinkError::InvalidDelegationActions(
                request.pubkey,
                "post-delegation actions attached to non-delegated clone target"
                    .to_string(),
            ));
        }

        let result = async {
            self.ensure_delegation_action_dependencies(
                request.pubkey,
                request.account.remote_slot(),
                &request.delegation_actions,
                fetch_context.clone(),
            )
            .await?;

            Ok(self
                .clone_account_with_ownership(
                    request.clone(),
                    fetch_context.clone(),
                )
                .await?)
        }
        .await;

        match result {
            Ok(signature) => Ok(signature),
            Err(err) => {
                let pubkey = request.pubkey;
                if self
                    .accounts_bank
                    .get_account(&pubkey)
                    .is_some_and(|account| account.undelegating())
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
                        fetch_context.clone(),
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

    pub(super) async fn clone_account_and_schedule_undelegation_with_ownership(
        &self,
        mut request: AccountCloneRequest,
        fetch_context: AccountFetchContext,
    ) -> ClonerResult<Signature> {
        let pubkey = request.pubkey;
        request.needs_undelegation = true;
        let remote_result = Self::clone_remote_result_for_request(&request);
        let clone_intent = Self::clone_intent_for_request(&request);
        let mut request = Some(request);

        loop {
            if self
                .accounts_bank
                .get_account(&pubkey)
                .is_some_and(|account| account.undelegating())
            {
                metrics::inc_chainlink_clone_accounts_total_with_context(
                    fetch_context.clone(),
                    remote_result,
                    clone_intent,
                    ChainlinkCloneOutcome::Skipped,
                );
                return Ok(Signature::default());
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
                        fetch_context.clone(),
                        remote_result,
                        clone_intent,
                        ChainlinkCloneOutcome::Submitted,
                    );
                    let is_empty_placeholder =
                        Self::is_empty_placeholder_account(
                            &owned_request.account,
                        );
                    let request_slot = owned_request.account.remote_slot();
                    Self::record_empty_placeholder_stage(
                        is_empty_placeholder,
                        fetch_context.clone(),
                        ChainlinkEmptyPlaceholderStage::CloneSubmitted,
                        Outcome::Success,
                    );
                    let result = self.cloner.clone_account(owned_request).await;
                    if result.is_ok() {
                        metrics::inc_chainlink_clone_accounts_total_with_context(
                            fetch_context.clone(),
                            remote_result,
                            clone_intent,
                            ChainlinkCloneOutcome::CloneSucceeded,
                        );
                        let materialization_outcome = self
                            .record_account_materialization(
                                &pubkey,
                                request_slot,
                                remote_result,
                                fetch_context.clone(),
                            );
                        Self::record_empty_placeholder_materialization_stage(
                            is_empty_placeholder,
                            fetch_context.clone(),
                            materialization_outcome,
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
                            fetch_context.clone(),
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

    pub(super) fn claim_undelegation(
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

    pub(super) fn normalize_unresolved_dlp_clone_request(
        &self,
        request: &mut AccountCloneRequest,
    ) -> ChainlinkResult<()> {
        if request.account.owner() != &dlp_api::id()
            || !request.account.delegated()
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

        if !request.delegation_actions.is_empty() {
            return Err(ChainlinkError::InvalidDelegationActions(
                request.pubkey,
                "post-delegation actions attached to unresolved DLP-owned clone target"
                    .to_string(),
            ));
        }

        request.account.set_delegated(false);
        request.account.set_confined(false);
        Ok(())
    }

    pub(super) fn local_delegated_clone_target_active(
        &self,
        pubkey: Pubkey,
    ) -> bool {
        self.accounts_bank
            .get_account(&pubkey)
            .is_some_and(|account| account.delegated())
    }
}
