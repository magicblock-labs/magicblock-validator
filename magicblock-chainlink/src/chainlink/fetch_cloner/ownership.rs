use super::*;

impl<T, U> FetchCloner<T, U>
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
{
    pub(super) async fn clone_account(
        &self,
        request: AccountCloneRequest,
        fetch_context: AccountFetchContext,
        materialization: AccountMaterialization,
    ) -> ClonerResult<MaterializedAccount> {
        let pubkey = request.pubkey;
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
            cloner::clone_account(&self.engine, request, materialization).await;
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
        result.map(|mode| MaterializedAccount { pubkey, mode })
    }

    pub(super) async fn watch_programdata(
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
        Ok(ProgramDataWatch::Installed)
    }

    pub(super) async fn unwatch_programdata(&self, program_id: Pubkey) {
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

    pub(super) async fn clone_program(
        &self,
        program: LoadedProgram,
        fetch_context: AccountFetchContext,
        materialization: AccountMaterialization,
    ) -> ClonerResult<Option<MaterializedAccount>> {
        let program_id = program.program_id;
        let remote_slot = program.remote_slot;
        let is_loaderv3 = matches!(program.loader, RemoteProgramLoader::V3);
        let remote_result = ChainlinkCloneRemoteResult::Found;
        let clone_intent = ChainlinkCloneIntent::ProgramData;

        if self
            .read_account(&program_id, |account| account.slot() >= remote_slot)
            .unwrap_or(false)
        {
            metrics::inc_chainlink_clone_accounts_total_with_context(
                fetch_context,
                remote_result,
                clone_intent,
                ChainlinkCloneOutcome::Skipped,
            );
            if is_loaderv3 {
                let _ = self.watch_programdata(program_id).await;
            }
            return Ok(None);
        }

        metrics::inc_chainlink_clone_accounts_total_with_context(
            fetch_context.clone(),
            remote_result,
            clone_intent,
            ChainlinkCloneOutcome::Submitted,
        );
        let installed_watch = is_loaderv3
            && matches!(
                self.watch_programdata(program_id).await,
                Ok(ProgramDataWatch::Installed)
            );
        let result =
            cloner::clone_program(&self.engine, program, materialization).await;
        if result.is_ok() {
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
        result.map(|mode| {
            mode.map(|mode| MaterializedAccount {
                pubkey: program_id,
                mode,
            })
        })
    }

    pub(super) async fn clone_account_with_post_delegation_action_invariants(
        &self,
        mut request: AccountCloneRequest,
        fetch_context: AccountFetchContext,
        materialization: AccountMaterialization,
    ) -> ChainlinkResult<Option<MaterializedAccount>> {
        if materialization == AccountMaterialization::Update
            && !self.contains_account(&request.pubkey)
        {
            trace!(
                pubkey = %request.pubkey,
                "Ignoring account update for account absent from bank"
            );
            return Ok(None);
        }

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
        self.normalize_immutable_account(&mut request);

        if materialization == AccountMaterialization::Update
            && request.post_delegation_mode.has_actions()
        {
            return Err(ChainlinkError::InvalidDelegationActions(
                request.pubkey,
                "account update unexpectedly contains post-delegation actions"
                    .to_string(),
            ));
        }

        if request.account.read().is(AccountMode::Delegated)
            && self.local_delegated_clone_target_active(request.pubkey)
        {
            return Ok(None);
        }

        let Some(delegation_actions) = request.post_delegation_mode.actions()
        else {
            return Ok(Some(
                self.clone_account(request, fetch_context, materialization)
                    .await?,
            ));
        };

        if !request.account.read().is(AccountMode::Delegated) {
            return Err(ChainlinkError::InvalidDelegationActions(
                request.pubkey,
                "post-delegation actions attached to non-delegated clone target"
                    .to_string(),
            ));
        }

        let result = async {
            self.ensure_delegation_action_dependencies(
                request.pubkey,
                request.account.read().slot(),
                delegation_actions,
                fetch_context.clone(),
            )
            .await?;

            Ok(Some(
                self.clone_account(
                    request.clone(),
                    fetch_context.clone(),
                    AccountMaterialization::Create,
                )
                .await?,
            ))
        }
        .await;

        match result {
            Ok(materialized) => Ok(materialized),
            Err(err) => {
                let pubkey = request.pubkey;
                warn!(
                    pubkey = %pubkey,
                    error = ?err,
                    "Post-delegation actions could not be satisfied; undelegating"
                );
                if self
                    .read_account(&pubkey, |account| {
                        account.is(AccountMode::Transient)
                    })
                    .unwrap_or(false)
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
                    .clone_account_and_schedule_undelegation(
                        request,
                        fetch_context,
                    )
                    .await
                {
                    Ok(materialized) => Ok(materialized),
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

    pub(super) async fn clone_account_and_schedule_undelegation(
        &self,
        mut request: AccountCloneRequest,
        fetch_context: AccountFetchContext,
    ) -> ClonerResult<Option<MaterializedAccount>> {
        let pubkey = request.pubkey;
        request.post_delegation_mode =
            ClonePostDelegationMode::RescueUndelegate;
        let remote_result = Self::clone_remote_result_for_request(&request);
        let clone_intent = Self::clone_intent_for_request(&request);

        if self
            .read_account(&pubkey, |account| account.is(AccountMode::Transient))
            .unwrap_or(false)
        {
            metrics::inc_chainlink_clone_accounts_total_with_context(
                fetch_context,
                remote_result,
                clone_intent,
                ChainlinkCloneOutcome::Skipped,
            );
            return Ok(None);
        }

        self.clone_account(
            request,
            fetch_context,
            AccountMaterialization::Create,
        )
        .await
        .map(Some)
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

    pub(super) fn normalize_immutable_account(
        &self,
        request: &mut AccountCloneRequest,
    ) {
        if !request.account.read().is(AccountMode::ReadOnly)
            && !request.account.read().is(AccountMode::Placeholder)
        {
            return;
        }

        let mode = immutable_account_mode(request.account.read().lamports());
        // Engine create composition can also refresh an existing target.
        // Placeholder is reserved for a zero-lamport account absent locally;
        // existing lifecycle transitions resolve through ReadOnly.
        let mode = if mode == AccountMode::Placeholder
            && !self.contains_account(&request.pubkey)
        {
            AccountMode::Placeholder
        } else {
            AccountMode::ReadOnly
        };
        if mode == request.account.read().mode() {
            return;
        }

        request.account = mem::take(&mut request.account).mode(mode);
    }

    pub(super) fn local_delegated_clone_target_active(
        &self,
        pubkey: Pubkey,
    ) -> bool {
        self.read_account(&pubkey, |account| account.is(AccountMode::Delegated))
            .unwrap_or(false)
    }
}
