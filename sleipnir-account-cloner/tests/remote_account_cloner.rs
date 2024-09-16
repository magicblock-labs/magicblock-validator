use std::collections::HashSet;

use sleipnir_account_cloner::{
    standard_blacklisted_accounts, AccountCloner, AccountClonerOutput,
    AccountClonerPermissions, AccountClonerUnclonableReason,
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

#[allow(clippy::too_many_arguments)]
fn setup_custom(
    internal_account_provider: InternalAccountProviderStub,
    account_fetcher: AccountFetcherStub,
    account_updates: AccountUpdatesStub,
    account_dumper: AccountDumperStub,
    allowed_program_ids: Option<HashSet<Pubkey>>,
    blacklisted_accounts: HashSet<Pubkey>,
    permissions: AccountClonerPermissions,
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
        allowed_program_ids,
        blacklisted_accounts,
        payer_init_lamports,
        permissions,
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

fn setup_replica(
    internal_account_provider: InternalAccountProviderStub,
    account_fetcher: AccountFetcherStub,
    account_updates: AccountUpdatesStub,
    account_dumper: AccountDumperStub,
    allowed_program_ids: Option<HashSet<Pubkey>>,
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
        allowed_program_ids,
        standard_blacklisted_accounts(&Pubkey::new_unique()),
        AccountClonerPermissions {
            allow_cloning_refresh: false,
            allow_cloning_new_accounts: true,
            allow_cloning_payer_accounts: true,
            allow_cloning_pda_accounts: true,
            allow_cloning_delegated_accounts: true,
            allow_cloning_program_accounts: true,
        },
    )
}

fn setup_programs_replica(
    internal_account_provider: InternalAccountProviderStub,
    account_fetcher: AccountFetcherStub,
    account_updates: AccountUpdatesStub,
    account_dumper: AccountDumperStub,
    allowed_program_ids: Option<HashSet<Pubkey>>,
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
        allowed_program_ids,
        standard_blacklisted_accounts(&Pubkey::new_unique()),
        AccountClonerPermissions {
            allow_cloning_refresh: false,
            allow_cloning_new_accounts: false,
            allow_cloning_payer_accounts: false,
            allow_cloning_pda_accounts: false,
            allow_cloning_delegated_accounts: false,
            allow_cloning_program_accounts: true,
        },
    )
}

fn setup_ephemeral(
    internal_account_provider: InternalAccountProviderStub,
    account_fetcher: AccountFetcherStub,
    account_updates: AccountUpdatesStub,
    account_dumper: AccountDumperStub,
    allowed_program_ids: Option<HashSet<Pubkey>>,
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
        allowed_program_ids,
        standard_blacklisted_accounts(&Pubkey::new_unique()),
        AccountClonerPermissions {
            allow_cloning_refresh: true,
            allow_cloning_new_accounts: true,
            allow_cloning_payer_accounts: true,
            allow_cloning_pda_accounts: true,
            allow_cloning_delegated_accounts: true,
            allow_cloning_program_accounts: true,
        },
    )
}

fn setup_offline(
    internal_account_provider: InternalAccountProviderStub,
    account_fetcher: AccountFetcherStub,
    account_updates: AccountUpdatesStub,
    account_dumper: AccountDumperStub,
    allowed_program_ids: Option<HashSet<Pubkey>>,
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
        allowed_program_ids,
        standard_blacklisted_accounts(&Pubkey::new_unique()),
        AccountClonerPermissions {
            allow_cloning_refresh: false,
            allow_cloning_new_accounts: false,
            allow_cloning_payer_accounts: false,
            allow_cloning_pda_accounts: false,
            allow_cloning_delegated_accounts: false,
            allow_cloning_program_accounts: false,
        },
    )
}

fn generate_payer_pubkey() -> Pubkey {
    loop {
        let pubkey = Pubkey::new_unique();
        if pubkey.is_on_curve() {
            return pubkey;
        }
    }
}

fn generate_pda_pubkey() -> Pubkey {
    loop {
        let pubkey = Pubkey::new_unique();
        if !pubkey.is_on_curve() {
            return pubkey;
        }
    }
}

#[tokio::test]
async fn test_clone_allow_new_account_payer_when_ephemeral() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_ephemeral(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // A new account that does not exist remotely
    let new_account = Pubkey::new_unique();
    account_fetcher.set_new_account(new_account, 42);
    // Run test
    let result = cloner.clone_account(&new_account).await;
    // Check expected result
    assert!(matches!(result, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&new_account), 1);
    assert!(account_updates.has_account_monitoring(&new_account));
    assert!(account_dumper.was_dumped_as_new_account(&new_account));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_allow_payer_account_when_ephemeral() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_ephemeral(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // A simple payer account
    let payer_account = generate_payer_pubkey();
    account_fetcher.set_payer_account(payer_account, 42);
    // Run test
    let result = cloner.clone_account(&payer_account).await;
    // Check expected result
    assert!(matches!(result, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&payer_account), 1);
    assert!(account_updates.has_account_monitoring(&payer_account));
    assert!(account_dumper.was_dumped_as_payer_account(&payer_account));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_allow_pda_account_when_ephemeral() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_ephemeral(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // A simple PDA account
    let pda_account = generate_pda_pubkey();
    account_fetcher.set_pda_account(pda_account, 42);
    // Run test
    let result = cloner.clone_account(&pda_account).await;
    // Check expected result
    assert!(matches!(result, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account), 1);
    assert!(account_updates.has_account_monitoring(&pda_account));
    assert!(account_dumper.was_dumped_as_pda_account(&pda_account));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_allow_delegated_account_when_ephemeral() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_ephemeral(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // A properly delegated account
    let delegated_account = generate_pda_pubkey();
    account_fetcher.set_delegated_account(delegated_account, 42, 11);
    // Run test
    let result = cloner.clone_account(&delegated_account).await;
    // Check expected result
    assert!(matches!(result, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&delegated_account), 1);
    assert!(account_updates.has_account_monitoring(&delegated_account));
    assert!(account_dumper.was_dumped_as_delegated_account(&delegated_account));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_allow_program_accounts_when_ephemeral() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_ephemeral(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // A simple program with its executable data alongside it
    let program_id = generate_pda_pubkey();
    let program_data = get_program_data_address(&program_id);
    let program_anchor = get_pubkey_anchor_idl(&program_id).unwrap();
    let program_shank = get_pubkey_shank_idl(&program_id).unwrap();
    account_fetcher.set_executable_account(program_id, 42);
    account_fetcher.set_pda_account(program_data, 42);
    account_fetcher.set_new_account(program_anchor, 42); // The anchor IDL does not exist, so it should use shank
    account_fetcher.set_pda_account(program_shank, 42);
    // Run test
    let result = cloner.clone_account(&program_id).await;
    // Check expected result
    assert!(matches!(result, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&program_id), 1);
    assert!(account_updates.has_account_monitoring(&program_id));
    assert!(account_dumper.was_dumped_as_program_id(&program_id));
    assert_eq!(account_fetcher.get_fetch_count(&program_data), 1);
    assert!(!account_updates.has_account_monitoring(&program_data));
    assert!(account_dumper.was_dumped_as_program_data(&program_data));
    assert_eq!(account_fetcher.get_fetch_count(&program_anchor), 1);
    assert!(!account_updates.has_account_monitoring(&program_anchor));
    assert!(account_dumper.was_untouched(&program_anchor));
    assert_eq!(account_fetcher.get_fetch_count(&program_shank), 1);
    assert!(!account_updates.has_account_monitoring(&program_shank));
    assert!(account_dumper.was_dumped_as_program_idl(&program_shank));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_program_accounts_when_ephemeral_with_whitelist() {
    // Important pubkeys
    let unallowed_program_id = generate_pda_pubkey();
    let allowed_program_id = generate_pda_pubkey();
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    let mut allowed_program_ids = HashSet::new();
    allowed_program_ids.insert(allowed_program_id);
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_ephemeral(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        Some(allowed_program_ids),
    );
    // Not allowed program account
    let unallowed_program_data =
        get_program_data_address(&unallowed_program_id);
    let unallowed_program_idl =
        get_pubkey_anchor_idl(&unallowed_program_id).unwrap();
    account_fetcher.set_executable_account(unallowed_program_id, 42);
    account_fetcher.set_pda_account(unallowed_program_data, 42);
    account_fetcher.set_pda_account(unallowed_program_idl, 42);
    // Run test
    let result = cloner.clone_account(&unallowed_program_id).await;
    // Check expected result
    assert!(matches!(
        result,
        Ok(AccountClonerOutput::Unclonable {
            reason: AccountClonerUnclonableReason::IsNotAllowedProgram,
            ..
        })
    ));
    assert_eq!(account_fetcher.get_fetch_count(&unallowed_program_id), 1);
    assert!(account_updates.has_account_monitoring(&unallowed_program_id));
    assert!(account_dumper.was_untouched(&unallowed_program_id));
    assert_eq!(account_fetcher.get_fetch_count(&unallowed_program_data), 0);
    assert!(!account_updates.has_account_monitoring(&unallowed_program_data));
    assert!(account_dumper.was_untouched(&unallowed_program_data));
    assert_eq!(account_fetcher.get_fetch_count(&unallowed_program_idl), 0);
    assert!(!account_updates.has_account_monitoring(&unallowed_program_idl));
    assert!(account_dumper.was_untouched(&unallowed_program_idl));
    // Allowed program accounts
    let allowed_program_data = get_program_data_address(&allowed_program_id);
    let allowed_program_idl =
        get_pubkey_anchor_idl(&allowed_program_id).unwrap();
    account_fetcher.set_executable_account(allowed_program_id, 52);
    account_fetcher.set_pda_account(allowed_program_data, 52);
    account_fetcher.set_pda_account(allowed_program_idl, 52);
    // Run test
    let result = cloner.clone_account(&allowed_program_id).await;
    // Check expected result
    assert!(matches!(result, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&allowed_program_id), 1);
    assert!(account_updates.has_account_monitoring(&allowed_program_id));
    assert!(account_dumper.was_dumped_as_program_id(&allowed_program_id));
    assert_eq!(account_fetcher.get_fetch_count(&allowed_program_data), 1);
    assert!(!account_updates.has_account_monitoring(&allowed_program_data));
    assert!(account_dumper.was_dumped_as_program_data(&allowed_program_data));
    assert_eq!(account_fetcher.get_fetch_count(&allowed_program_idl), 1);
    assert!(!account_updates.has_account_monitoring(&allowed_program_idl));
    assert!(account_dumper.was_dumped_as_program_idl(&allowed_program_idl));
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
    let (cloner, cancellation_token, worker_handle) = setup_ephemeral(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // The account is already in the bank and should not be cloned under any circumstances
    let already_in_the_bank = Pubkey::new_unique();
    internal_account_provider.set(already_in_the_bank, Default::default());
    // Run test
    let result = cloner.clone_account(&already_in_the_bank).await;
    // Check expected result
    assert!(matches!(
        result,
        Ok(AccountClonerOutput::Unclonable {
            reason: AccountClonerUnclonableReason::AlreadyLocallyOverriden,
            ..
        })
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
    let (cloner, cancellation_token, worker_handle) = setup_ephemeral(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // The remote clock is blacklisted by default on our ephemeral
    let blacklisted_account = clock::ID;
    // Run test
    let result = cloner.clone_account(&blacklisted_account).await;
    // Check expected result
    assert!(matches!(
        result,
        Ok(AccountClonerOutput::Unclonable {
            reason: AccountClonerUnclonableReason::IsBlacklisted,
            ..
        })
    ));
    assert_eq!(account_fetcher.get_fetch_count(&blacklisted_account), 0);
    assert!(!account_updates.has_account_monitoring(&blacklisted_account));
    assert!(account_dumper.was_untouched(&blacklisted_account));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_refuse_new_account_when_programs_replica() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_programs_replica(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // An account that doesnt exist on remote chain
    let new_account = Pubkey::new_unique();
    account_fetcher.set_new_account(new_account, 42);
    // Run test 1
    let result = cloner.clone_account(&new_account).await;
    // Check expected result
    assert!(matches!(
        result,
        Ok(AccountClonerOutput::Unclonable {
            reason: AccountClonerUnclonableReason::DisallowNewAccount,
            ..
        })
    ));
    assert_eq!(account_fetcher.get_fetch_count(&new_account), 1);
    assert!(!account_updates.has_account_monitoring(&new_account));
    assert!(account_dumper.was_untouched(&new_account));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_refuse_payer_account_when_programs_replica() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_programs_replica(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // A payer account
    let payer_account = generate_payer_pubkey();
    account_fetcher.set_payer_account(payer_account, 42);
    // Run test
    let result = cloner.clone_account(&payer_account).await;
    // Check expected result
    assert!(matches!(
        result,
        Ok(AccountClonerOutput::Unclonable {
            reason: AccountClonerUnclonableReason::DisallowPayerAccount,
            ..
        })
    ));
    assert_eq!(account_fetcher.get_fetch_count(&payer_account), 1);
    assert!(!account_updates.has_account_monitoring(&payer_account));
    assert!(account_dumper.was_untouched(&payer_account));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_refuse_pda_account_when_programs_replica() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_programs_replica(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // A simple account that is not delegated and not a program (a PDA)
    let pda_account = generate_pda_pubkey();
    account_fetcher.set_pda_account(pda_account, 42);
    // Run test
    let result = cloner.clone_account(&pda_account).await;
    // Check expected result
    assert!(matches!(
        result,
        Ok(AccountClonerOutput::Unclonable {
            reason: AccountClonerUnclonableReason::DisallowPdaAccount,
            ..
        })
    ));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account), 1);
    assert!(!account_updates.has_account_monitoring(&pda_account));
    assert!(account_dumper.was_untouched(&pda_account));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_refuse_delegated_account_when_programs_replica() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_programs_replica(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // A properly delegated account
    let delegated_account = generate_pda_pubkey();
    account_fetcher.set_delegated_account(delegated_account, 42, 11);
    // Run test
    let result = cloner.clone_account(&delegated_account).await;
    // Check expected result
    assert!(matches!(
        result,
        Ok(AccountClonerOutput::Unclonable {
            reason: AccountClonerUnclonableReason::DisallowDelegatedAccount,
            ..
        })
    ));
    assert_eq!(account_fetcher.get_fetch_count(&delegated_account), 1);
    assert!(!account_updates.has_account_monitoring(&delegated_account));
    assert!(account_dumper.was_untouched(&delegated_account));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_allow_program_accounts_when_programs_replica() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_programs_replica(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // A simple program with its executable data alongside it
    let program_id = generate_pda_pubkey();
    let program_data = get_program_data_address(&program_id);
    let program_anchor = get_pubkey_anchor_idl(&program_id).unwrap();
    let program_shank = get_pubkey_shank_idl(&program_id).unwrap();
    account_fetcher.set_executable_account(program_id, 42);
    account_fetcher.set_pda_account(program_data, 42);
    account_fetcher.set_new_account(program_anchor, 42); // The anchor IDL does not exist, so it should use shank
    account_fetcher.set_pda_account(program_shank, 42);
    // Run test
    let result = cloner.clone_account(&program_id).await;
    // Check expected result
    assert!(matches!(result, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&program_id), 1);
    assert!(!account_updates.has_account_monitoring(&program_id));
    assert!(account_dumper.was_dumped_as_program_id(&program_id));
    assert_eq!(account_fetcher.get_fetch_count(&program_data), 1);
    assert!(!account_updates.has_account_monitoring(&program_data));
    assert!(account_dumper.was_dumped_as_program_data(&program_data));
    assert_eq!(account_fetcher.get_fetch_count(&program_anchor), 1);
    assert!(!account_updates.has_account_monitoring(&program_anchor));
    assert!(account_dumper.was_untouched(&program_anchor));
    assert_eq!(account_fetcher.get_fetch_count(&program_shank), 1);
    assert!(!account_updates.has_account_monitoring(&program_shank));
    assert!(account_dumper.was_dumped_as_program_idl(&program_shank));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_allow_pda_account_when_replica() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_replica(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // A simple pda account
    let pda_account = generate_pda_pubkey();
    account_fetcher.set_pda_account(pda_account, 42);
    // Run test
    let result = cloner.clone_account(&pda_account).await;
    // Check expected result
    assert!(matches!(result, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account), 1);
    assert!(!account_updates.has_account_monitoring(&pda_account));
    assert!(account_dumper.was_dumped_as_pda_account(&pda_account));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_allow_payer_account_when_replica() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_replica(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // A payer account
    let payer_account = generate_payer_pubkey();
    account_fetcher.set_payer_account(payer_account, 42);
    // Run test
    let result = cloner.clone_account(&payer_account).await;
    // Check expected result
    assert!(matches!(result, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&payer_account), 1);
    assert!(!account_updates.has_account_monitoring(&payer_account));
    assert!(account_dumper.was_dumped_as_payer_account(&payer_account));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_refuse_any_account_when_offline() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_offline(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // A simple account that is initially undelegated that will become delegated during the test
    let payer_account = generate_payer_pubkey();
    let pda_account = generate_pda_pubkey();
    let program_id = generate_pda_pubkey();
    let program_data = get_program_data_address(&program_id);
    let program_idl = get_pubkey_anchor_idl(&program_id).unwrap();
    account_fetcher.set_payer_account(payer_account, 42);
    account_fetcher.set_pda_account(pda_account, 42);
    account_fetcher.set_executable_account(program_id, 42);
    account_fetcher.set_pda_account(program_data, 42);
    account_fetcher.set_pda_account(program_idl, 42);
    // Run test
    let result1 = cloner.clone_account(&payer_account).await;
    // Check expected result1
    assert!(matches!(
        result1,
        Ok(AccountClonerOutput::Unclonable {
            reason: AccountClonerUnclonableReason::NoCloningAllowed,
            ..
        })
    ));
    assert_eq!(account_fetcher.get_fetch_count(&payer_account), 0);
    assert!(!account_updates.has_account_monitoring(&payer_account));
    assert!(account_dumper.was_untouched(&payer_account));
    // Run test
    let result2 = cloner.clone_account(&pda_account).await;
    // Check expected result2
    assert!(matches!(
        result2,
        Ok(AccountClonerOutput::Unclonable {
            reason: AccountClonerUnclonableReason::NoCloningAllowed,
            ..
        })
    ));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account), 0);
    assert!(!account_updates.has_account_monitoring(&pda_account));
    assert!(account_dumper.was_untouched(&pda_account));
    // Run test
    let result3 = cloner.clone_account(&program_id).await;
    // Check expected result3
    assert!(matches!(
        result3,
        Ok(AccountClonerOutput::Unclonable {
            reason: AccountClonerUnclonableReason::NoCloningAllowed,
            ..
        })
    ));
    assert_eq!(account_fetcher.get_fetch_count(&program_id), 0);
    assert!(!account_updates.has_account_monitoring(&program_id));
    assert!(account_dumper.was_untouched(&program_id));
    assert_eq!(account_fetcher.get_fetch_count(&program_data), 0);
    assert!(!account_updates.has_account_monitoring(&program_data));
    assert!(account_dumper.was_untouched(&program_data));
    assert_eq!(account_fetcher.get_fetch_count(&program_idl), 0);
    assert!(!account_updates.has_account_monitoring(&program_idl));
    assert!(account_dumper.was_untouched(&program_idl));
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
    let (cloner, cancellation_token, worker_handle) = setup_ephemeral(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // A simple program with its executable data alongside it
    let program_id = generate_pda_pubkey();
    let program_data = get_program_data_address(&program_id);
    let program_idl = get_pubkey_anchor_idl(&program_id).unwrap();
    account_fetcher.set_executable_account(program_id, 42);
    account_fetcher.set_pda_account(program_data, 42);
    account_fetcher.set_pda_account(program_idl, 42);
    // Run test (cloned at the same time for the same thing, must run once and share the result)
    let future1 = cloner.clone_account(&program_id);
    let future2 = cloner.clone_account(&program_id);
    let future3 = cloner.clone_account(&program_id);
    let result1 = future1.await;
    let result2 = future2.await;
    let result3 = future3.await;
    // Check expected results
    assert!(matches!(result1, Ok(AccountClonerOutput::Cloned { .. })));
    assert!(matches!(result2, Ok(AccountClonerOutput::Cloned { .. })));
    assert!(matches!(result3, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&program_id), 1);
    assert!(account_updates.has_account_monitoring(&program_id));
    assert!(account_dumper.was_dumped_as_program_id(&program_id));
    assert_eq!(account_fetcher.get_fetch_count(&program_data), 1);
    assert!(!account_updates.has_account_monitoring(&program_data));
    assert!(account_dumper.was_dumped_as_program_data(&program_data));
    assert_eq!(account_fetcher.get_fetch_count(&program_idl), 1);
    assert!(!account_updates.has_account_monitoring(&program_idl));
    assert!(account_dumper.was_dumped_as_program_idl(&program_idl));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_properly_cached_pda_account_when_ephemeral() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_ephemeral(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // A simple PDA account
    let pda_account = generate_pda_pubkey();
    account_fetcher.set_pda_account(pda_account, 42);
    // Run test (we clone the account for the first time)
    let result1 = cloner.clone_account(&pda_account).await;
    // Check expected result1
    assert!(matches!(result1, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account), 1);
    assert!(account_updates.has_account_monitoring(&pda_account));
    assert!(account_dumper.was_dumped_as_pda_account(&pda_account));
    // Clear dump history
    account_dumper.clear_history();
    // Run test (we re-clone the account and it should be in the cache)
    let result2 = cloner.clone_account(&pda_account).await;
    // Check expected result2
    assert!(matches!(result2, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account), 1);
    assert!(account_updates.has_account_monitoring(&pda_account));
    assert!(account_dumper.was_untouched(&pda_account));
    // The account is now updated remotely
    account_updates.set_known_update_slot(pda_account, 66);
    // Run test (we re-clone the account and it should clear the cache and re-dump)
    let result3 = cloner.clone_account(&pda_account).await;
    // Check expected result3
    assert!(matches!(result3, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account), 2);
    assert!(account_updates.has_account_monitoring(&pda_account));
    assert!(account_dumper.was_dumped_as_pda_account(&pda_account));
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
    let (cloner, cancellation_token, worker_handle) = setup_ephemeral(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // A simple program
    let program_id = generate_pda_pubkey();
    let program_data = get_program_data_address(&program_id);
    let program_idl_pubkey = get_pubkey_anchor_idl(&program_id).unwrap();
    account_fetcher.set_executable_account(program_id, 42);
    account_fetcher.set_pda_account(program_data, 42);
    account_fetcher.set_pda_account(program_idl_pubkey, 42);
    // Run test (we clone the account for the first time)
    let result1 = cloner.clone_account(&program_id).await;
    // Check expected result1
    assert!(matches!(result1, Ok(AccountClonerOutput::Cloned { .. })));
    // Check expected result1
    assert_eq!(account_fetcher.get_fetch_count(&program_id), 1);
    assert!(account_updates.has_account_monitoring(&program_id));
    assert!(account_dumper.was_dumped_as_program_id(&program_id));
    assert_eq!(account_fetcher.get_fetch_count(&program_data), 1);
    assert!(!account_updates.has_account_monitoring(&program_data));
    assert!(account_dumper.was_dumped_as_program_data(&program_data));
    assert_eq!(account_fetcher.get_fetch_count(&program_idl_pubkey), 1);
    assert!(!account_updates.has_account_monitoring(&program_idl_pubkey));
    assert!(account_dumper.was_dumped_as_program_idl(&program_idl_pubkey));
    // Clear dump history
    account_dumper.clear_history();
    // Run test (we re-clone the account and it should be in the cache)
    let result2 = cloner.clone_account(&program_id).await;
    // Check expected result2
    assert!(matches!(result2, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&program_id), 1);
    assert!(account_updates.has_account_monitoring(&program_id));
    assert!(account_dumper.was_untouched(&program_id));
    assert_eq!(account_fetcher.get_fetch_count(&program_data), 1);
    assert!(!account_updates.has_account_monitoring(&program_data));
    assert!(account_dumper.was_untouched(&program_data));
    assert_eq!(account_fetcher.get_fetch_count(&program_idl_pubkey), 1);
    assert!(!account_updates.has_account_monitoring(&program_idl_pubkey));
    assert!(account_dumper.was_untouched(&program_idl_pubkey));
    // The account is now updated remotely
    account_updates.set_known_update_slot(program_id, 66);
    // Run test (we re-clone the account and it should clear the cache and re-dump)
    let result3 = cloner.clone_account(&program_id).await;
    // Check expected result3
    assert!(matches!(result3, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&program_id), 2);
    assert!(account_updates.has_account_monitoring(&program_id));
    assert!(account_dumper.was_dumped_as_program_id(&program_id));
    assert_eq!(account_fetcher.get_fetch_count(&program_data), 2);
    assert!(!account_updates.has_account_monitoring(&program_data));
    assert!(account_dumper.was_dumped_as_program_data(&program_data));
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
    let (cloner, cancellation_token, worker_handle) = setup_ephemeral(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // A properly delegated account at first (delegation status will change during the test)
    let pda_account = generate_pda_pubkey();
    account_fetcher.set_delegated_account(pda_account, 42, 11);
    // Run test (we clone the account for the first time as delegated)
    let result1 = cloner.clone_account(&pda_account).await;
    // Check expected result1
    assert!(matches!(result1, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account), 1);
    assert!(account_updates.has_account_monitoring(&pda_account));
    assert!(account_dumper.was_dumped_as_delegated_account(&pda_account));
    // Clear dump history
    account_dumper.clear_history();
    // Run test (we re-clone the account and it should be in the cache)
    let result2 = cloner.clone_account(&pda_account).await;
    // Check expected result3
    assert!(matches!(result2, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account), 1);
    assert!(account_updates.has_account_monitoring(&pda_account));
    assert!(account_dumper.was_untouched(&pda_account));
    // The account is now updated remotely (but its delegation status didnt change)
    account_updates.set_known_update_slot(pda_account, 66);
    // Run test (we MUST NOT re-dump)
    let result3 = cloner.clone_account(&pda_account).await;
    // Check expected result3
    assert!(matches!(result3, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account), 2);
    assert!(account_updates.has_account_monitoring(&pda_account));
    assert!(account_dumper.was_untouched(&pda_account));
    // The account is now updated remotely (AND IT BECOMES UNDELEGATED)
    account_updates.set_known_update_slot(pda_account, 77);
    account_fetcher.set_pda_account(pda_account, 77);
    // Run test (now we MUST RE-DUMP as an undelegated account)
    let result4 = cloner.clone_account(&pda_account).await;
    // Check expected result4
    assert!(matches!(result4, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account), 3);
    assert!(account_updates.has_account_monitoring(&pda_account));
    assert!(account_dumper.was_dumped_as_pda_account(&pda_account));
    // Clear dump history
    account_dumper.clear_history();
    // The account is now updated remotely (AND IT BECOMES RE-DELEGATED)
    account_updates.set_known_update_slot(pda_account, 88);
    account_fetcher.set_delegated_account(pda_account, 88, 88);
    // Run test (now we MUST RE-DUMP as an delegated account)
    let result5 = cloner.clone_account(&pda_account).await;
    // Check expected result5
    assert!(matches!(result5, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account), 4);
    assert!(account_updates.has_account_monitoring(&pda_account));
    assert!(account_dumper.was_dumped_as_delegated_account(&pda_account));
    // Clear dump history
    account_dumper.clear_history();
    // The account is now re-delegated from a different slot
    account_updates.set_known_update_slot(pda_account, 99);
    account_fetcher.set_delegated_account(pda_account, 99, 99);
    // Run test (now we MUST RE-DUMP as an delegated account because the delegation_slot changed, even if delegation status DIDNT)
    let result6 = cloner.clone_account(&pda_account).await;
    // Check expected result6
    assert!(matches!(result6, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account), 5);
    assert!(account_updates.has_account_monitoring(&pda_account));
    assert!(account_dumper.was_dumped_as_delegated_account(&pda_account));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}

#[tokio::test]
async fn test_clone_properly_upgrading_downgrading_when_created_and_deleted() {
    // Stubs
    let internal_account_provider = InternalAccountProviderStub::default();
    let account_fetcher = AccountFetcherStub::default();
    let account_updates = AccountUpdatesStub::default();
    let account_dumper = AccountDumperStub::default();
    // Create account cloner worker and client
    let (cloner, cancellation_token, worker_handle) = setup_ephemeral(
        internal_account_provider.clone(),
        account_fetcher.clone(),
        account_updates.clone(),
        account_dumper.clone(),
        None,
    );
    // A simple account that initially doesnt exist but we will create it after the clone
    let pda_account = generate_pda_pubkey();
    account_fetcher.set_new_account(pda_account, 42);
    // Run test (we clone the account for the first time)
    let result1 = cloner.clone_account(&pda_account).await;
    // Check expected result1
    assert!(matches!(result1, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account), 1);
    assert!(account_updates.has_account_monitoring(&pda_account));
    assert!(account_dumper.was_dumped_as_new_account(&pda_account));
    // Clear dump history
    account_dumper.clear_history();
    // Run test (we re-clone the account and it should be in the cache)
    let result2 = cloner.clone_account(&pda_account).await;
    // Check expected result2
    assert!(matches!(result2, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account), 1);
    assert!(account_updates.has_account_monitoring(&pda_account));
    assert!(account_dumper.was_untouched(&pda_account));
    // The account is now updated remotely, as it becomes a pda account (not new anymore)
    account_fetcher.set_pda_account(pda_account, 66);
    account_updates.set_known_update_slot(pda_account, 66);
    // Run test (we re-clone the account and it should clear the cache and re-dump)
    let result3 = cloner.clone_account(&pda_account).await;
    // Check expected result3
    assert!(matches!(result3, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account), 2);
    assert!(account_updates.has_account_monitoring(&pda_account));
    assert!(account_dumper.was_dumped_as_pda_account(&pda_account));
    // Clear dump history
    account_dumper.clear_history();
    // Run test (we re-clone the account and it should be in the cache)
    let result4 = cloner.clone_account(&pda_account).await;
    // Check expected result4
    assert!(matches!(result4, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account), 2);
    assert!(account_updates.has_account_monitoring(&pda_account));
    assert!(account_dumper.was_untouched(&pda_account));
    // The account is now removed/closed remotely
    account_fetcher.set_new_account(pda_account, 77);
    account_updates.set_known_update_slot(pda_account, 77);
    // Run test (we re-clone the account and it should clear the cache and re-dump)
    let result5 = cloner.clone_account(&pda_account).await;
    // Check expected result5
    assert!(matches!(result5, Ok(AccountClonerOutput::Cloned { .. })));
    assert_eq!(account_fetcher.get_fetch_count(&pda_account), 3);
    assert!(account_updates.has_account_monitoring(&pda_account));
    assert!(account_dumper.was_dumped_as_new_account(&pda_account));
    // Clear dump history
    account_dumper.clear_history();
    // Run test (we re-clone the account and it should be in the cache)
    let result6 = cloner.clone_account(&pda_account).await;
    assert!(matches!(result6, Ok(AccountClonerOutput::Cloned { .. })));
    // Check expected result6
    assert_eq!(account_fetcher.get_fetch_count(&pda_account), 3);
    assert!(account_updates.has_account_monitoring(&pda_account));
    assert!(account_dumper.was_untouched(&pda_account));
    // Cleanup everything correctly
    cancellation_token.cancel();
    assert!(worker_handle.await.is_ok());
}
