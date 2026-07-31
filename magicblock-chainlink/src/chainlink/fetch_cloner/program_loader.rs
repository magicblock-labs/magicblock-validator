use std::time::{Duration, Instant};

use magicblock_accounts_db::traits::AccountsBank;
use magicblock_metrics::metrics::{
    AccountFetchContext, AccountFetchReason, ChainlinkCompanionFetchKind,
};
use solana_account::{AccountSharedData, ReadableAccount};
use solana_loader_v3_interface::state::UpgradeableLoaderState;
use solana_pubkey::Pubkey;
use tracing::*;

use super::{
    log_companion_fetch_failure, subscription::release_program_data_subs,
    CompanionFetchLogContext, FetchCloner, ProgramVerifyState,
};
use crate::{
    cloner::Cloner,
    remote_account_provider::{
        program_account::{
            get_loaderv3_get_program_data_address, ProgramAccountResolver,
            LOADER_V1, LOADER_V2, LOADER_V3,
        },
        ChainPubsubClient, ChainRpcClient, SubscriptionReason,
    },
};

/// Minimum spacing between remote verifications of an already-loaded
/// LoaderV3 program. Providers emit a notification for heavily-invoked
/// program accounts on every mainnet slot without anything changing;
/// this bounds upgrade-detection latency for such programs to roughly
/// the window, at a cost of one ~45-byte header fetch per window.
#[cfg(not(test))]
const PROGRAM_VERIFY_THROTTLE: Duration = Duration::from_secs(60);
#[cfg(test)]
const PROGRAM_VERIFY_THROTTLE: Duration = Duration::from_millis(300);

/// Length of the programdata metadata prefix (tag + deploy slot +
/// authority) the header check compares.
fn programdata_header_len() -> usize {
    UpgradeableLoaderState::size_of_programdata_metadata()
}

/// Schedules a single verification at throttle-window expiry so
/// notifications suppressed within the window are never lost — without
/// this, an upgrade of a rarely-invoked program could go undetected
/// indefinitely because no later notification would re-trigger a check.
fn schedule_deferred_verify<T, U, V, C>(
    this: &FetchCloner<T, U, V, C>,
    pubkey: Pubkey,
    account: &AccountSharedData,
    companion_fetch_log_context: &CompanionFetchLogContext,
    elapsed: Duration,
) where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    {
        let mut cache = this.program_verify_cache.lock();
        match cache.get_mut(&pubkey) {
            Some(state) if !state.deferred_verify => {
                state.deferred_verify = true;
            }
            // Already scheduled or state gone: nothing to do.
            _ => return,
        }
    }
    let this = this.clone();
    let account = account.clone();
    let ctx = companion_fetch_log_context.clone();
    let delay = PROGRAM_VERIFY_THROTTLE.saturating_sub(elapsed)
        + Duration::from_millis(10);
    tokio::task::spawn(async move {
        tokio::time::sleep(delay).await;
        if let Some(state) = this.program_verify_cache.lock().get_mut(&pubkey) {
            state.deferred_verify = false;
        }
        // Box-erased to break the async type cycle with the handler.
        let fut: std::pin::Pin<
            Box<dyn std::future::Future<Output = ()> + Send>,
        > = Box::pin(handle_executable_sub_update_with_context(
            &this, pubkey, account, &ctx,
        ));
        fut.await;
    });
}

/// Resolves a subscription update for a program this validator already
/// loaded without re-pulling the full program data. Returns `true` when
/// the update needs no further handling: immutable programs carry no
/// information, and LoaderV3 programs are verified against a ~45-byte
/// programdata header fetch (throttled, with a deferred check covering
/// suppressed notifications). Any doubt returns `false` so the caller
/// runs the unchanged full-reload path.
async fn resolve_update_for_loaded_program<T, U, V, C>(
    this: &FetchCloner<T, U, V, C>,
    pubkey: Pubkey,
    account: &AccountSharedData,
    program_data_pubkey: Pubkey,
    companion_fetch_log_context: &CompanionFetchLogContext,
) -> bool
where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    let Some(state) = this.program_verify_cache.lock().get(&pubkey).cloned()
    else {
        // Not loaded by this process yet: full load.
        return false;
    };
    if this.accounts_bank.get_account(&pubkey).is_none() {
        // The bank copy was evicted since the load that populated the
        // cache; drop the stale entry and reload.
        this.program_verify_cache.lock().pop(&pubkey);
        return false;
    }

    if account.owner().eq(&LOADER_V2) {
        // LoaderV2 programs are immutable once deployed.
        trace!(pubkey = %pubkey, "Ignoring update for immutable LoaderV2 program");
        return true;
    }
    if !account.owner().eq(&LOADER_V3) {
        return false;
    }
    let elapsed = state.verified_at.elapsed();
    if elapsed < PROGRAM_VERIFY_THROTTLE {
        // Suppressed, but never lost: a single verification covering the
        // window's notifications runs at window expiry.
        schedule_deferred_verify(
            this,
            pubkey,
            account,
            companion_fetch_log_context,
            elapsed,
        );
        return true;
    }
    let Some(loaded_header) = state.programdata_header else {
        return false;
    };

    match this
        .remote_account_provider
        .get_account_data_slice(
            &program_data_pubkey,
            0,
            programdata_header_len(),
            account.remote_slot(),
        )
        .await
    {
        // Comparing the raw metadata prefix catches deploy-slot changes
        // (upgrades) and authority changes alike. Responses are truncated
        // to the prefix in case a provider ignores `dataSlice`.
        Ok(Some(header))
            if header.get(..programdata_header_len()).unwrap_or(&header)
                == loaded_header =>
        {
            trace!(pubkey = %pubkey, "Programdata header unchanged; skipping program reload");
            if let Some(state) =
                this.program_verify_cache.lock().get_mut(&pubkey)
            {
                state.verified_at = Instant::now();
            }
            true
        }
        // Header changed, or programdata closed: reload so the bank
        // reflects remote state.
        Ok(_) => false,
        Err(err) => {
            warn!(
                pubkey = %pubkey,
                program_data = %program_data_pubkey,
                error = %err,
                "Programdata header check failed; falling back to full program reload"
            );
            false
        }
    }
}

pub(crate) async fn handle_executable_sub_update_with_context<T, U, V, C>(
    this: &FetchCloner<T, U, V, C>,
    pubkey: Pubkey,
    account: AccountSharedData,
    companion_fetch_log_context: &CompanionFetchLogContext,
) where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    if !this.is_program_allowed(&pubkey) {
        debug!(pubkey = %pubkey, "Skipping clone of program, not in allowed_programs");
        return;
    }

    if account.owner().eq(&LOADER_V1) {
        // This is a program deployed on chain with BPFLoader1111111111111111111111111111111111.
        // By definition it cannot be upgraded, hence we should never get a subscription
        // update for it.
        error!(pubkey = %pubkey, "Unexpected subscription update for program loaded with LoaderV1");
        return;
    }

    // For LoaderV3 programs we need to fetch the program data account
    let program_data_pubkey = get_loaderv3_get_program_data_address(&pubkey);
    let account_owner = *account.owner();

    if resolve_update_for_loaded_program(
        this,
        pubkey,
        &account,
        program_data_pubkey,
        companion_fetch_log_context,
    )
    .await
    {
        return;
    }

    let acquired_program_data_reason = if account.owner().eq(&LOADER_V3) {
        this.acquire_subscription_reason(
            &program_data_pubkey,
            SubscriptionReason::ProgramData,
        )
        .await
        .map(|_| true)
        .unwrap_or_else(|err| {
            warn!(
                pubkey = %program_data_pubkey,
                error = ?err,
                "Failed to acquire program data subscription reason"
            );
            false
        })
    } else {
        false
    };
    let program_load_context = AccountFetchContext::subscription_update(
        AccountFetchReason::ProgramLoad,
    );
    let program_data_context = program_load_context
        .clone()
        .with_reason(AccountFetchReason::ProgramData);

    let (program_account, program_data_account) =
        if account.owner().eq(&LOADER_V3) {
            match FetchCloner::task_to_fetch_with_program_data(
                this,
                pubkey,
                account.remote_slot(),
                program_data_context,
            )
            .await
            {
                Ok(Ok(account_with_companion)) => (
                    account_with_companion.account.into_account_shared_data(),
                    account_with_companion
                        .companion_account
                        .map(|x| x.into_account_shared_data()),
                ),
                Ok(Err(err)) => {
                    log_companion_fetch_failure(
                        companion_fetch_log_context,
                        program_data_pubkey,
                        ChainlinkCompanionFetchKind::ProgramData,
                        &err,
                    );
                    if acquired_program_data_reason {
                        // Both refs exist for LoaderV3 program-data cleanup.
                        release_program_data_subs(
                            &this.remote_account_provider,
                            program_data_pubkey,
                        )
                        .await;
                    }
                    return;
                }
                Err(err) => {
                    log_companion_fetch_failure(
                        companion_fetch_log_context,
                        program_data_pubkey,
                        ChainlinkCompanionFetchKind::ProgramData,
                        &err,
                    );
                    if acquired_program_data_reason {
                        // Both refs exist for LoaderV3 program-data cleanup.
                        release_program_data_subs(
                            &this.remote_account_provider,
                            program_data_pubkey,
                        )
                        .await;
                    }
                    return;
                }
            }
        } else {
            (account, None::<AccountSharedData>)
        };

    let fetched_programdata_header = program_data_account
        .as_ref()
        .and_then(|pd| pd.data().get(..programdata_header_len()))
        .map(|header| header.to_vec());

    let loaded_program = match ProgramAccountResolver::try_new(
        pubkey,
        *program_account.owner(),
        Some(program_account),
        program_data_account,
    ) {
        Ok(x) => x.into_loaded_program(),
        Err(err) => {
            warn!(pubkey = %pubkey, error = %err, "Failed to resolve program account into bank");
            if acquired_program_data_reason {
                // Both refs exist for LoaderV3 program-data cleanup.
                release_program_data_subs(
                    &this.remote_account_provider,
                    program_data_pubkey,
                )
                .await;
            }
            return;
        }
    };
    match this
        .clone_program_with_ownership(loaded_program, program_load_context)
        .await
    {
        // Only successful loads are remembered: a failure leaves the next
        // notification free to retry immediately, as before.
        Ok(_) => {
            this.program_verify_cache.lock().put(
                pubkey,
                ProgramVerifyState {
                    programdata_header: account_owner
                        .eq(&LOADER_V3)
                        .then_some(fetched_programdata_header)
                        .flatten(),
                    verified_at: Instant::now(),
                    deferred_verify: false,
                },
            );
        }
        Err(err) => {
            warn!(pubkey = %pubkey, error = %err, "Failed to clone program into bank");
        }
    }

    if acquired_program_data_reason {
        // Both refs exist for LoaderV3 program-data cleanup.
        release_program_data_subs(
            &this.remote_account_provider,
            program_data_pubkey,
        )
        .await;
    }
}
