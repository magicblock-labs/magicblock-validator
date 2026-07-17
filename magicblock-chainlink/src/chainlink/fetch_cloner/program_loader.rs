use magicblock_accounts_db::traits::AccountsBank;
use magicblock_metrics::metrics::{
    AccountFetchContext, AccountFetchReason, ChainlinkCompanionFetchKind,
};
use solana_account::{AccountSharedData, ReadableAccount};
use solana_pubkey::Pubkey;
use tracing::*;

use super::{
    log_companion_fetch_failure, subscription::release_program_data_subs,
    CompanionFetchLogContext, FetchCloner,
};
use crate::{
    cloner::Cloner,
    remote_account_provider::{
        program_account::{
            get_loaderv3_get_program_data_address, ProgramAccountResolver,
            LOADER_V1, LOADER_V3,
        },
        ChainPubsubClient, ChainRpcClient, SubscriptionReason,
    },
};

#[cfg(test)]
pub(crate) async fn handle_executable_sub_update<T, U, V, C>(
    this: &FetchCloner<T, U, V, C>,
    pubkey: Pubkey,
    account: AccountSharedData,
) where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
{
    let companion_fetch_log_context = CompanionFetchLogContext {
        origin: AccountFetchContext::subscription_update(
            AccountFetchReason::SubscriptionUpdateClone,
        ),
        primary_pubkey: pubkey,
        context_slot: Some(account.remote_slot()),
    };
    handle_executable_sub_update_with_context(
        this,
        pubkey,
        account,
        &companion_fetch_log_context,
    )
    .await;
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
    let program_data_context =
        program_load_context.with_reason(AccountFetchReason::ProgramData);

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
    if let Err(err) = this
        .clone_program_with_ownership(loaded_program, program_load_context)
        .await
    {
        warn!(pubkey = %pubkey, error = %err, "Failed to clone program into bank");
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
