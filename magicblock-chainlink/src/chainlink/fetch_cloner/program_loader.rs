use magicblock_core::traits::AccountsBank;
use magicblock_metrics::metrics::AccountFetchOrigin;
use solana_account::{AccountSharedData, ReadableAccount};
use solana_pubkey::Pubkey;
use tracing::*;

use super::FetchCloner;
use crate::{
    cloner::Cloner,
    remote_account_provider::{
        photon_client::PhotonClient,
        program_account::{ProgramAccountResolver, LOADER_V1, LOADER_V3},
        ChainPubsubClient, ChainRpcClient,
    },
};

pub(crate) async fn handle_executable_sub_update<T, U, V, C, P>(
    this: &FetchCloner<T, U, V, C, P>,
    pubkey: Pubkey,
    account: AccountSharedData,
) where
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
    P: PhotonClient,
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
    let (program_account, program_data_account) = if account
        .owner()
        .eq(&LOADER_V3)
    {
        match FetchCloner::task_to_fetch_with_program_data(
            this,
            pubkey,
            account.remote_slot(),
            AccountFetchOrigin::GetAccount,
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
                error!(pubkey = %pubkey, error = %err, "Failed to fetch program data account");
                return;
            }
            Err(err) => {
                error!(pubkey = %pubkey, error = %err, "Failed to fetch program data account");
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
            error!(pubkey = %pubkey, error = %err, "Failed to resolve program account into bank");
            return;
        }
    };
    if let Err(err) = this.cloner.clone_program(loaded_program).await {
        error!(pubkey = %pubkey, error = %err, "Failed to clone program into bank");
    }
}
