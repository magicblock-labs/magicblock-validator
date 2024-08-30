use std::collections::HashSet;

use sleipnir_account_cloner::{
    RemoteAccountClonerClient, RemoteAccountClonerWorker,
};
use sleipnir_account_dumper::AccountDumperStub;
use sleipnir_account_fetcher::AccountFetcherStub;
use sleipnir_account_updates::AccountUpdatesStub;
use sleipnir_accounts_api::InternalAccountProviderStub;
use solana_sdk::native_token::LAMPORTS_PER_SOL;
use tokio_util::sync::CancellationToken;

fn setup(
    internal_account_provider: InternalAccountProviderStub,
    account_fetcher: AccountFetcherStub,
    account_updates: AccountUpdatesStub,
    account_dumper: AccountDumperStub,
) -> (
    RemoteAccountClonerClient,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    // Default configuration
    let blacklisted_accounts = HashSet::new();
    let payer_init_lamports = Some(1_000 * LAMPORTS_PER_SOL);
    let allow_non_programs_undelegated = true;
    // Create account cloner worker and client
    let mut cloner_worker = RemoteAccountClonerWorker::new(
        internal_account_provider,
        account_fetcher,
        account_updates,
        account_dumper,
        blacklisted_accounts,
        payer_init_lamports,
        allow_non_programs_undelegated,
    );
    let cloner_client = RemoteAccountClonerClient::new(&cloner_worker);
    // Run the worker in a separate task
    let cancellation_token = CancellationToken::new();
    let cloner_worker_handle = {
        let cloner_cancellation_token = cancellation_token.clone();
        tokio::spawn(async move {
            cloner_worker
                .start_clone_request_processing(cloner_cancellation_token)
                .await
        })
    };
    // Ready to run
    (cloner_client, cancellation_token, cloner_worker_handle)
}

#[tokio::test]
async fn test_devnet_clone_invalid_and_valid() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (client, cancellation_token, worker_handle) = setup(
        internal_account_provider,
        account_fetcher,
        account_updates,
        account_dumper,
    );

    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}
