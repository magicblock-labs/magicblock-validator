use std::collections::HashSet;

use sleipnir_account_cloner::{
    standard_blacklisted_accounts, AccountCloner, AccountClonerOutput,
    RemoteAccountClonerClient, RemoteAccountClonerWorker,
};
use sleipnir_account_dumper::AccountDumperStub;
use sleipnir_account_fetcher::AccountFetcherStub;
use sleipnir_account_updates::AccountUpdatesStub;
use sleipnir_accounts_api::InternalAccountProviderStub;
use solana_sdk::{
    bpf_loader_upgradeable::get_program_data_address,
    native_token::LAMPORTS_PER_SOL, pubkey::Pubkey, sysvar::clock,
};
use tokio_util::sync::CancellationToken;

fn setup_custom(
    internal_account_provider: InternalAccountProviderStub,
    account_fetcher: AccountFetcherStub,
    account_updates: AccountUpdatesStub,
    account_dumper: AccountDumperStub,
    blacklisted_accounts: HashSet<Pubkey>,
    allow_non_programs_undelegated: bool,
) -> (
    RemoteAccountClonerClient,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    // Default configuration
    let payer_init_lamports = Some(1_000 * LAMPORTS_PER_SOL);
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

fn setup_ephem(
    internal_account_provider: InternalAccountProviderStub,
    account_fetcher: AccountFetcherStub,
    account_updates: AccountUpdatesStub,
    account_dumper: AccountDumperStub,
) -> (
    RemoteAccountClonerClient,
    CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    setup_custom(
        internal_account_provider,
        account_fetcher,
        account_updates,
        account_dumper,
        standard_blacklisted_accounts(&Pubkey::new_unique()),
        true,
    )
}

#[tokio::test]
async fn test_clone_simple_system_account() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_ephem(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
    );
    // A simple account owned by the system program
    let system_account_pubkey = Pubkey::new_unique();
    account_fetcher.set_system_account(system_account_pubkey, 42);
    // Run test
    let result = cloner.clone_account(&system_account_pubkey).await;
    assert!(result.is_ok());
    // Check expected result
    assert!(matches!(result.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&system_account_pubkey), 1);
    assert!(account_updates.has_account_monitoring(&system_account_pubkey));
    assert!(account_dumper.was_dumped_as_system_account(&system_account_pubkey));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_simple_pda_account() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_ephem(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
    );
    // A simple account with data that's not owned by the system program (a PDA)
    let pda_account_pubkey = Pubkey::new_unique();
    account_fetcher.set_pda_account(pda_account_pubkey, 42);
    // Run test
    let result = cloner.clone_account(&pda_account_pubkey).await;
    assert!(result.is_ok());
    // Check expected result
    assert!(matches!(result.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account_pubkey), 1);
    assert!(account_updates.has_account_monitoring(&pda_account_pubkey));
    assert!(account_dumper.was_dumped_as_pda_account(&pda_account_pubkey));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_simple_delegated_account() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_ephem(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
    );
    // A properly delegated account
    let delegated_account_pubkey = Pubkey::new_unique();
    account_fetcher.set_delegated_account(
        delegated_account_pubkey,
        Pubkey::new_unique(),
        42,
    );
    // Run test
    let result = cloner.clone_account(&delegated_account_pubkey).await;
    assert!(result.is_ok());
    // Check expected result
    assert!(matches!(result.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(
        account_fetcher.get_fetch_count(&delegated_account_pubkey),
        1
    );
    assert!(account_updates.has_account_monitoring(&delegated_account_pubkey));
    assert!(account_dumper
        .was_dumped_as_delegated_account(&delegated_account_pubkey));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_simple_program_accounts() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_ephem(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
    );
    // A simple program with its executable data alongside it
    let program_id_pubkey = Pubkey::new_unique();
    let program_data_pubkey = get_program_data_address(&program_id_pubkey);
    account_fetcher.set_executable_account(program_id_pubkey, 42);
    account_fetcher.set_pda_account(program_data_pubkey, 42);
    // Run test
    let result = cloner.clone_account(&program_id_pubkey).await;
    eprintln!("result:{:?}", result);
    assert!(result.is_ok());
    // Check expected result
    assert!(matches!(result.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&program_id_pubkey), 1);
    assert!(account_updates.has_account_monitoring(&program_id_pubkey));
    assert!(account_dumper.was_dumped_as_program_id(&program_id_pubkey));
    assert_eq!(account_fetcher.get_fetch_count(&program_data_pubkey), 1);
    assert!(!account_updates.has_account_monitoring(&program_data_pubkey));
    assert!(account_dumper.was_dumped_as_program_data(&program_data_pubkey));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_refuse_already_written_in_bank() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_ephem(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
    );
    // The account is already in the bank and should not be cloned under any circumstances
    let already_in_the_bank = Pubkey::new_unique();
    internal_account_provider.set(already_in_the_bank, Default::default());
    // Run test
    let result = cloner.clone_account(&already_in_the_bank).await;
    assert!(result.is_ok());
    // Check expected result
    assert!(matches!(
        result.unwrap(),
        AccountClonerOutput::Unclonable(_)
    ));
    assert_eq!(account_fetcher.get_fetch_count(&already_in_the_bank), 0);
    assert!(!account_updates.has_account_monitoring(&already_in_the_bank));
    assert!(account_dumper.was_untouched(&already_in_the_bank));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_refuse_blacklisted_account() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_ephem(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
    );
    // The remote clock is blacklisted by default on our ephemeral
    let blacklisted_account = clock::ID;
    // Run test
    let result = cloner.clone_account(&blacklisted_account).await;
    assert!(result.is_ok());
    // Check expected result
    assert!(matches!(
        result.unwrap(),
        AccountClonerOutput::Unclonable(_)
    ));
    assert_eq!(account_fetcher.get_fetch_count(&blacklisted_account), 0);
    assert!(!account_updates.has_account_monitoring(&blacklisted_account));
    assert!(account_dumper.was_untouched(&blacklisted_account));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_ignore_non_programs_undelegated_when_configured() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_custom(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        HashSet::new(),
        false,
    );
    // A simple account that is not delegated and not a program (a PDA or system account)
    let pda_account_pubkey = Pubkey::new_unique();
    account_fetcher.set_pda_account(pda_account_pubkey, Default::default());
    // Run test
    let result = cloner.clone_account(&pda_account_pubkey).await;
    assert!(result.is_ok());
    // Check expected result
    assert!(matches!(result.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account_pubkey), 1);
    assert!(account_updates.has_account_monitoring(&pda_account_pubkey));
    assert!(account_dumper.was_untouched(&pda_account_pubkey));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_will_not_fetch_the_same_thing_multiple_times() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_ephem(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
    );
    // A simple program with its executable data alongside it
    let program_id_pubkey = Pubkey::new_unique();
    let program_data_pubkey = get_program_data_address(&program_id_pubkey);
    account_fetcher.set_executable_account(program_id_pubkey, 42);
    account_fetcher.set_pda_account(program_data_pubkey, 42);
    // Run test
    let future1 = cloner.clone_account(&program_id_pubkey);
    let future2 = cloner.clone_account(&program_id_pubkey);
    let result1 = future1.await;
    let result2 = future2.await;
    assert!(result1.is_ok());
    assert!(result2.is_ok());
    // Check expected result
    assert!(matches!(result1.unwrap(), AccountClonerOutput::Cloned(_)));
    assert!(matches!(result2.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&program_id_pubkey), 1);
    assert!(account_updates.has_account_monitoring(&program_id_pubkey));
    assert!(account_dumper.was_dumped_as_program_id(&program_id_pubkey));
    assert_eq!(account_fetcher.get_fetch_count(&program_data_pubkey), 1);
    assert!(!account_updates.has_account_monitoring(&program_data_pubkey));
    assert!(account_dumper.was_dumped_as_program_data(&program_data_pubkey));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}
