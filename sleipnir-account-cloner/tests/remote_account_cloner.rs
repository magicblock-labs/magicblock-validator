use std::collections::HashSet;

use sleipnir_account_cloner::{
    standard_blacklisted_accounts, AccountCloner, AccountClonerOutput,
    RemoteAccountClonerClient, RemoteAccountClonerWorker,
};
use sleipnir_account_dumper::AccountDumperStub;
use sleipnir_account_fetcher::AccountFetcherStub;
use sleipnir_account_updates::AccountUpdatesStub;
use sleipnir_accounts_api::InternalAccountProviderStub;
use sleipnir_mutator::idl::{get_pubkey_anchor_idl, get_pubkey_shank_idl};
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
async fn test_clone_simple_new_account() {
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
    // A new account that does not exist remotely
    let new_account_pubkey = Pubkey::new_unique();
    // Run test
    let result = cloner.clone_account(&new_account_pubkey).await;
    assert!(result.is_ok());
    // Check expected result
    assert!(matches!(result.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&new_account_pubkey), 1);
    assert!(account_updates.has_account_monitoring(&new_account_pubkey));
    assert!(account_dumper.was_untouched(&new_account_pubkey));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
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
    account_fetcher.set_delegated_account(delegated_account_pubkey, 42, 11);
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
    let program_idl_pubkey = get_pubkey_shank_idl(&program_id_pubkey).unwrap();
    account_fetcher.set_executable_account(program_id_pubkey, 42);
    account_fetcher.set_pda_account(program_data_pubkey, 42);
    account_fetcher.set_pda_account(program_idl_pubkey, 42);
    // Run test
    let result = cloner.clone_account(&program_id_pubkey).await;
    assert!(result.is_ok());
    // Check expected result
    assert!(matches!(result.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&program_id_pubkey), 1);
    assert!(account_updates.has_account_monitoring(&program_id_pubkey));
    assert!(account_dumper.was_dumped_as_program_id(&program_id_pubkey));
    assert_eq!(account_fetcher.get_fetch_count(&program_data_pubkey), 1);
    assert!(!account_updates.has_account_monitoring(&program_data_pubkey));
    assert!(account_dumper.was_dumped_as_program_data(&program_data_pubkey));
    assert_eq!(account_fetcher.get_fetch_count(&program_idl_pubkey), 1);
    assert!(!account_updates.has_account_monitoring(&program_idl_pubkey));
    assert!(account_dumper.was_dumped_as_program_idl(&program_idl_pubkey));
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
    let program_idl_pubkey = get_pubkey_anchor_idl(&program_id_pubkey).unwrap();
    account_fetcher.set_executable_account(program_id_pubkey, 42);
    account_fetcher.set_pda_account(program_data_pubkey, 42);
    account_fetcher.set_pda_account(program_idl_pubkey, 42);
    // Run test (cloned at the same time for the same thing, must run once and share the result)
    let future1 = cloner.clone_account(&program_id_pubkey);
    let future2 = cloner.clone_account(&program_id_pubkey);
    let future3 = cloner.clone_account(&program_id_pubkey);
    let result1 = future1.await;
    let result2 = future2.await;
    let result3 = future3.await;
    assert!(result1.is_ok());
    assert!(result2.is_ok());
    assert!(result3.is_ok());
    // Check expected result
    assert!(matches!(result1.unwrap(), AccountClonerOutput::Cloned(_)));
    assert!(matches!(result2.unwrap(), AccountClonerOutput::Cloned(_)));
    assert!(matches!(result3.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&program_id_pubkey), 1);
    assert!(account_updates.has_account_monitoring(&program_id_pubkey));
    assert!(account_dumper.was_dumped_as_program_id(&program_id_pubkey));
    assert_eq!(account_fetcher.get_fetch_count(&program_data_pubkey), 1);
    assert!(!account_updates.has_account_monitoring(&program_data_pubkey));
    assert!(account_dumper.was_dumped_as_program_data(&program_data_pubkey));
    assert_eq!(account_fetcher.get_fetch_count(&program_idl_pubkey), 1);
    assert!(!account_updates.has_account_monitoring(&program_idl_pubkey));
    assert!(account_dumper.was_dumped_as_program_idl(&program_idl_pubkey));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_properly_cached_pda_account() {
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
    // A simple account not owned by the system program
    let pda_account_pubkey = Pubkey::new_unique();
    account_fetcher.set_pda_account(pda_account_pubkey, 42);
    // Run test (we clone the account for the first time)
    let result1 = cloner.clone_account(&pda_account_pubkey).await;
    assert!(result1.is_ok());
    // Check expected result1
    assert!(matches!(result1.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account_pubkey), 1);
    assert!(account_updates.has_account_monitoring(&pda_account_pubkey));
    assert!(account_dumper.was_dumped_as_pda_account(&pda_account_pubkey));
    // Clear dump history
    account_dumper.clear_history();
    // Run test (we re-clone the account and it should be in the cache)
    let result2 = cloner.clone_account(&pda_account_pubkey).await;
    assert!(result2.is_ok());
    assert!(matches!(result2.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account_pubkey), 1);
    assert!(account_updates.has_account_monitoring(&pda_account_pubkey));
    assert!(account_dumper.was_untouched(&pda_account_pubkey));
    // The account is now updated remotely
    account_updates.set_known_update_slot(pda_account_pubkey, 66);
    // Run test (we re-clone the account and it should clear the cache and re-dump)
    let result3 = cloner.clone_account(&pda_account_pubkey).await;
    assert!(result3.is_ok());
    assert!(matches!(result3.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account_pubkey), 2);
    assert!(account_updates.has_account_monitoring(&pda_account_pubkey));
    assert!(account_dumper.was_dumped_as_pda_account(&pda_account_pubkey));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_properly_cached_program() {
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
    // A simple program
    let program_id_pubkey = Pubkey::new_unique();
    let program_data_pubkey = get_program_data_address(&program_id_pubkey);
    let program_idl_pubkey = get_pubkey_anchor_idl(&program_id_pubkey).unwrap();
    account_fetcher.set_executable_account(program_id_pubkey, 42);
    account_fetcher.set_pda_account(program_data_pubkey, 42);
    account_fetcher.set_pda_account(program_idl_pubkey, 42);
    // Run test (we clone the account for the first time)
    let result1 = cloner.clone_account(&program_id_pubkey).await;
    assert!(result1.is_ok());
    // Check expected result1
    assert!(matches!(result1.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&program_id_pubkey), 1);
    assert!(account_updates.has_account_monitoring(&program_id_pubkey));
    assert!(account_dumper.was_dumped_as_program_id(&program_id_pubkey));
    assert_eq!(account_fetcher.get_fetch_count(&program_data_pubkey), 1);
    assert!(!account_updates.has_account_monitoring(&program_data_pubkey));
    assert!(account_dumper.was_dumped_as_program_data(&program_data_pubkey));
    assert_eq!(account_fetcher.get_fetch_count(&program_idl_pubkey), 1);
    assert!(!account_updates.has_account_monitoring(&program_idl_pubkey));
    assert!(account_dumper.was_dumped_as_program_idl(&program_idl_pubkey));
    // Clear dump history
    account_dumper.clear_history();
    // Run test (we re-clone the account and it should be in the cache)
    let result2 = cloner.clone_account(&program_id_pubkey).await;
    assert!(result2.is_ok());
    assert!(matches!(result2.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&program_id_pubkey), 1);
    assert!(account_updates.has_account_monitoring(&program_id_pubkey));
    assert!(account_dumper.was_untouched(&program_id_pubkey));
    assert_eq!(account_fetcher.get_fetch_count(&program_data_pubkey), 1);
    assert!(!account_updates.has_account_monitoring(&program_data_pubkey));
    assert!(account_dumper.was_untouched(&program_data_pubkey));
    assert_eq!(account_fetcher.get_fetch_count(&program_idl_pubkey), 1);
    assert!(!account_updates.has_account_monitoring(&program_idl_pubkey));
    assert!(account_dumper.was_untouched(&program_idl_pubkey));
    // The account is now updated remotely
    account_updates.set_known_update_slot(program_id_pubkey, 66);
    // Run test (we re-clone the account and it should clear the cache and re-dump)
    let result3 = cloner.clone_account(&program_id_pubkey).await;
    assert!(result3.is_ok());
    assert!(matches!(result3.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&program_id_pubkey), 2);
    assert!(account_updates.has_account_monitoring(&program_id_pubkey));
    assert!(account_dumper.was_dumped_as_program_id(&program_id_pubkey));
    assert_eq!(account_fetcher.get_fetch_count(&program_data_pubkey), 2);
    assert!(!account_updates.has_account_monitoring(&program_data_pubkey));
    assert!(account_dumper.was_dumped_as_program_data(&program_data_pubkey));
    assert_eq!(account_fetcher.get_fetch_count(&program_idl_pubkey), 2);
    assert!(!account_updates.has_account_monitoring(&program_idl_pubkey));
    assert!(account_dumper.was_dumped_as_program_idl(&program_idl_pubkey));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_properly_cached_delegated_account_that_changes_state() {
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
    // A properly delegated account at first (delegation status will change during the test)
    let changed_account_pubkey = Pubkey::new_unique();
    account_fetcher.set_delegated_account(changed_account_pubkey, 42, 11);
    // Run test (we clone the account for the first time as delegated)
    let result1 = cloner.clone_account(&changed_account_pubkey).await;
    assert!(result1.is_ok());
    // Check expected result1
    assert!(matches!(result1.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&changed_account_pubkey), 1);
    assert!(account_updates.has_account_monitoring(&changed_account_pubkey));
    assert!(
        account_dumper.was_dumped_as_delegated_account(&changed_account_pubkey)
    );
    // Clear dump history
    account_dumper.clear_history();
    // Run test (we re-clone the account and it should be in the cache)
    let result2 = cloner.clone_account(&changed_account_pubkey).await;
    assert!(result2.is_ok());
    assert!(matches!(result2.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&changed_account_pubkey), 1);
    assert!(account_updates.has_account_monitoring(&changed_account_pubkey));
    assert!(account_dumper.was_untouched(&changed_account_pubkey));
    // The account is now updated remotely (but its delegation status didnt change)
    account_updates.set_known_update_slot(changed_account_pubkey, 66);
    // Run test (we MUST NOT re-dump)
    let result3 = cloner.clone_account(&changed_account_pubkey).await;
    assert!(result3.is_ok());
    assert!(matches!(result3.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&changed_account_pubkey), 2);
    assert!(account_updates.has_account_monitoring(&changed_account_pubkey));
    assert!(account_dumper.was_untouched(&changed_account_pubkey));
    // The account is now updated remotely (AND IT BECOMES UNDELEGATED)
    account_updates.set_known_update_slot(changed_account_pubkey, 77);
    account_fetcher.set_pda_account(changed_account_pubkey, 77);
    // Run test (now we MUST RE-DUMP as an undelegated account)
    let result4 = cloner.clone_account(&changed_account_pubkey).await;
    assert!(result4.is_ok());
    assert!(matches!(result4.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&changed_account_pubkey), 3);
    assert!(account_updates.has_account_monitoring(&changed_account_pubkey));
    assert!(account_dumper.was_dumped_as_pda_account(&changed_account_pubkey));
    // Clear dump history
    account_dumper.clear_history();
    // The account is now updated remotely (AND IT BECOMES RE-DELEGATED)
    account_updates.set_known_update_slot(changed_account_pubkey, 88);
    account_fetcher.set_delegated_account(changed_account_pubkey, 88, 88);
    // Run test (now we MUST RE-DUMP as an delegated account)
    let result5 = cloner.clone_account(&changed_account_pubkey).await;
    assert!(result5.is_ok());
    assert!(matches!(result5.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&changed_account_pubkey), 4);
    assert!(account_updates.has_account_monitoring(&changed_account_pubkey));
    assert!(
        account_dumper.was_dumped_as_delegated_account(&changed_account_pubkey)
    );
    // Clear dump history
    account_dumper.clear_history();
    // The account is now re-delegated from a different slot
    account_updates.set_known_update_slot(changed_account_pubkey, 99);
    account_fetcher.set_delegated_account(changed_account_pubkey, 99, 99);
    // Run test (now we MUST RE-DUMP as an delegated account because the delegation_slot changed, even if delegation status DIDNT)
    let result6 = cloner.clone_account(&changed_account_pubkey).await;
    assert!(result6.is_ok());
    assert!(matches!(result6.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&changed_account_pubkey), 5);
    assert!(account_updates.has_account_monitoring(&changed_account_pubkey));
    assert!(
        account_dumper.was_dumped_as_delegated_account(&changed_account_pubkey)
    );
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_properly_upgrading_new_account_when_it_gets_created() {
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
    // A simple account that initially doesnt exist but we will create it after the clone
    let created_account_pubkey = Pubkey::new_unique();
    // Run test (we clone the account for the first time)
    let result1 = cloner.clone_account(&created_account_pubkey).await;
    assert!(result1.is_ok());
    // Check expected result1
    assert!(matches!(result1.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&created_account_pubkey), 1);
    assert!(account_updates.has_account_monitoring(&created_account_pubkey));
    assert!(account_dumper.was_untouched(&created_account_pubkey));
    // Clear dump history
    account_dumper.clear_history();
    // Run test (we re-clone the account and it should be in the cache)
    let result2 = cloner.clone_account(&created_account_pubkey).await;
    assert!(result2.is_ok());
    assert!(matches!(result2.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&created_account_pubkey), 1);
    assert!(account_updates.has_account_monitoring(&created_account_pubkey));
    assert!(account_dumper.was_untouched(&created_account_pubkey));
    // The account is now updated remotely, as it becomes as pda account (not new anymore)
    account_fetcher.set_pda_account(created_account_pubkey, 66);
    account_updates.set_known_update_slot(created_account_pubkey, 66);
    // Run test (we re-clone the account and it should clear the cache and re-dump)
    let result3 = cloner.clone_account(&created_account_pubkey).await;
    assert!(result3.is_ok());
    assert!(matches!(result3.unwrap(), AccountClonerOutput::Cloned(_)));
    assert_eq!(account_fetcher.get_fetch_count(&created_account_pubkey), 2);
    assert!(account_updates.has_account_monitoring(&created_account_pubkey));
    assert!(account_dumper.was_dumped_as_pda_account(&created_account_pubkey));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}
