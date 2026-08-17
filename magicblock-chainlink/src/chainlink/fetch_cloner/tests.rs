use std::sync::{Arc, atomic::AtomicU64};

use magicblock_config::config::LifecycleMode;
use solana_account::{Account, AccountBuilder, AccountMode};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;

use super::*;

type TestFetchCloner = FetchCloner<ChainRpcClientMock, ChainPubsubClientMock>;

use crate::{
    cloner::{AccountCloneRequest, ClonePostDelegationMode, DelegationActions},
    remote_account_provider::{
        RemoteAccountProvider,
        chain_pubsub_client::mock::ChainPubsubClientMock,
        config::RemoteAccountProviderConfig,
    },
    testing::{
        rpc_client_mock::{ChainRpcClientMock, ChainRpcClientMockBuilder},
        utils::create_test_subscribed_accounts_with_config,
    },
};

fn request(account: AccountBuilder) -> AccountCloneRequest {
    AccountCloneRequest {
        pubkey: Pubkey::new_unique(),
        account,
        commit_frequency_ms: None,
        post_delegation_mode: ClonePostDelegationMode::None,
        delegated_to_other: None,
    }
}

fn account() -> AccountBuilder {
    AccountBuilder::from(Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: system_program::id(),
        executable: false,
        rent_epoch: 0,
    })
    .mode(AccountMode::ReadOnly)
}

fn delegation_record_account(delegation_slot: u64) -> Account {
    let record = DelegationRecord {
        authority: Pubkey::new_unique(),
        owner: Pubkey::new_unique(),
        delegation_slot,
        lamports: 1_000_000,
        commit_frequency_ms: 1_000,
    };
    let mut data = vec![0; DelegationRecord::size_with_discriminator()];
    record.to_bytes_with_discriminator(&mut data).unwrap();
    Account {
        data,
        owner: dlp_api::id(),
        ..Default::default()
    }
}

fn undelegation_request_account(
    delegated_account: Pubkey,
    expires_at_slot: u64,
) -> Account {
    let request = UndelegationRequest {
        delegated_account,
        expires_at_slot,
    };
    let mut data = vec![0; UndelegationRequest::size_with_discriminator()];
    request.to_bytes_with_discriminator(&mut data).unwrap();
    Account {
        data,
        owner: dlp_api::id(),
        ..Default::default()
    }
}

#[test]
fn replayed_requests_require_local_or_confined_authority() {
    let validator = Pubkey::new_unique();
    let mut record = DelegationRecord {
        authority: validator,
        owner: Pubkey::new_unique(),
        delegation_slot: 1,
        lamports: 1,
        commit_frequency_ms: 1,
    };

    assert!(delegation_record_has_local_authority(&record, validator));
    record.authority = Pubkey::default();
    assert!(delegation_record_has_local_authority(&record, validator));
    record.authority = Pubkey::new_unique();
    assert!(!delegation_record_has_local_authority(&record, validator));
}

#[test]
fn replay_recovery_record_scan_is_slot_and_authority_bounded() {
    let authority = Pubkey::new_unique();
    let config = replay_recovery_record_config(authority, 42);

    assert_eq!(config.account_config.min_context_slot, Some(42));
    assert_eq!(
        config.account_config.data_slice.unwrap().length,
        DelegationRecord::size_with_discriminator()
    );
    let filters = config.filters.unwrap();
    assert_eq!(filters.len(), 2);
    assert!(matches!(
        &filters[1],
        RpcFilterType::Memcmp(memcmp)
            if memcmp.offset() == 8
                && memcmp.bytes().is_some_and(|bytes| {
                    bytes.as_slice() == authority.as_ref()
                })
    ));
}

#[test]
fn replay_recovery_selects_only_records_created_inside_gap() {
    let before = Pubkey::new_unique();
    let inside = Pubkey::new_unique();
    let through = Pubkey::new_unique();
    let after = Pubkey::new_unique();
    let range = ReplayRecoveryRange {
        after_slot: 10,
        through_slot: 20,
    };

    let records = records_in_replay_gap(
        [
            (before, delegation_record_account(10)),
            (inside, delegation_record_account(11)),
            (through, delegation_record_account(20)),
            (after, delegation_record_account(21)),
        ],
        range,
    )
    .unwrap();

    assert_eq!(records[&11], HashSet::from([inside]));
    assert_eq!(records[&20], HashSet::from([through]));
    assert_eq!(records.len(), 2);
}

#[test]
fn replay_recovery_requests_full_confirmed_blocks() {
    let config = replay_recovery_block_config();

    assert_eq!(config.encoding, Some(UiTransactionEncoding::Base64));
    assert_eq!(config.transaction_details, Some(TransactionDetails::Full));
    assert_eq!(config.rewards, Some(false));
    assert_eq!(config.max_supported_transaction_version, Some(1));
}

#[test]
fn incomplete_replay_recovery_retries_are_bounded() {
    let incomplete = ChainlinkError::IncompleteReplayRecovery(1);

    assert_eq!(next_replay_recovery_retry_attempt(&incomplete, 0), Some(1));
    assert_eq!(next_replay_recovery_retry_attempt(&incomplete, 1), Some(2));
    assert_eq!(next_replay_recovery_retry_attempt(&incomplete, 2), None);
    assert_eq!(
        next_replay_recovery_retry_attempt(
            &ChainlinkError::IncompleteUndelegationRequestRecovery(1),
            2,
        ),
        Some(2)
    );
}

#[test]
fn replay_recovery_request_scan_is_slot_and_type_bounded() {
    let config = undelegation_request_config(42, Some(&[0xab, 0xcd]));

    assert_eq!(config.account_config.min_context_slot, Some(42));
    assert!(matches!(
        config.filters.as_deref(),
        Some([
            RpcFilterType::DataSize(size),
            RpcFilterType::Memcmp(memcmp),
            RpcFilterType::Memcmp(prefix),
        ]) if *size == UndelegationRequest::size_with_discriminator() as u64
            && memcmp.offset() == 0
            && memcmp.bytes().is_some_and(|bytes| {
                bytes.as_slice()
                    == AccountDiscriminator::UndelegationRequest
                        .to_bytes()
                        .as_slice()
            })
            && prefix.offset() == 8
            && prefix.bytes().is_some_and(|bytes| {
                bytes.as_slice() == [0xab, 0xcd]
            })
    ));

    let global = undelegation_request_config(42, None);
    assert_eq!(global.filters.unwrap().len(), 2);
}

#[tokio::test]
async fn request_scan_recursively_partitions_failed_bucket() {
    let rpc_client = ChainRpcClientMockBuilder::new().slot(42).build();
    let (updates_sender, updates_receiver) = mpsc::channel(100);
    let pubsub_client =
        ChainPubsubClientMock::new(updates_sender, updates_receiver);
    let (forward_sender, _forward_receiver) = mpsc::channel(100);
    let config = RemoteAccountProviderConfig::default_with_lifecycle_mode(
        LifecycleMode::Ephemeral,
    );
    let subscribed_accounts =
        create_test_subscribed_accounts_with_config(&config);
    let remote_account_provider =
        RemoteAccountProvider::try_from_clients_and_mode(
            rpc_client.clone(),
            pubsub_client,
            forward_sender,
            &config,
            subscribed_accounts,
            Arc::<AtomicU64>::default(),
        )
        .await
        .unwrap()
        .unwrap();
    let validator_pubkey = Pubkey::new_unique();
    let delegated_account = Pubkey::new_unique();
    let record_pda =
        delegation_record_pda_from_delegated_account(&delegated_account);
    let request_pda =
        undelegation_request_pda_from_delegated_account(&delegated_account);
    let mut record = delegation_record_account(1);
    let record_data = DelegationRecord {
        authority: validator_pubkey,
        owner: Pubkey::new_unique(),
        delegation_slot: 1,
        lamports: 1,
        commit_frequency_ms: 1,
    };
    record_data
        .to_bytes_with_discriminator(&mut record.data)
        .unwrap();
    rpc_client.add_account(record_pda, record);
    rpc_client.add_account(
        request_pda,
        undelegation_request_account(delegated_account, 100),
    );
    rpc_client.fail_unpartitioned_undelegation_request_scan();
    rpc_client.fail_single_byte_undelegation_request_prefix(
        delegated_account.to_bytes()[0],
    );
    let baseline = rpc_client.program_account_fetches();

    let scan = scan_undelegation_requests_with_provider(
        &remote_account_provider,
        validator_pubkey,
        0,
    )
    .await
    .expect("recursively partitioned request scan should succeed");

    assert_eq!(scan.incomplete_partitions, 0);
    assert_eq!(
        scan.requests,
        vec![ObservedUndelegationRequest {
            request_pda,
            delegated_account,
            expires_at_slot: 100,
            observed_slot: 42,
        }]
    );
    assert_eq!(rpc_client.program_account_fetches() - baseline, 515);

    rpc_client.fail_all_undelegation_request_scans();
    let baseline = rpc_client.program_account_fetches();
    let unavailable = scan_undelegation_requests_with_provider(
        &remote_account_provider,
        validator_pubkey,
        0,
    )
    .await
    .expect("systemic scan failure should remain a partial result");

    assert!(unavailable.requests.is_empty());
    assert_eq!(unavailable.incomplete_partitions, 1);
    assert_eq!(rpc_client.program_account_fetches() - baseline, 2);
}

#[test]
fn clone_request_classification() {
    let empty = request(AccountBuilder::default());
    assert!(TestFetchCloner::is_empty_placeholder_account(
        &empty.account
    ));
    assert_eq!(
        TestFetchCloner::clone_remote_result_for_request(&empty),
        ChainlinkCloneRemoteResult::NotFound
    );
    assert_eq!(
        TestFetchCloner::clone_intent_for_request(&empty),
        ChainlinkCloneIntent::EmptyPlaceholder
    );

    let normal = request(account());
    assert!(!TestFetchCloner::is_empty_placeholder_account(
        &normal.account
    ));
    assert_eq!(
        TestFetchCloner::clone_remote_result_for_request(&normal),
        ChainlinkCloneRemoteResult::Found
    );
    assert_eq!(
        TestFetchCloner::clone_intent_for_request(&normal),
        ChainlinkCloneIntent::NormalAccount
    );

    let delegated_account = account().mode(AccountMode::Delegated);
    let delegated = request(delegated_account);
    assert_eq!(
        TestFetchCloner::clone_intent_for_request(&delegated),
        ChainlinkCloneIntent::DelegationRecord
    );

    let mut dependency = request(account());
    dependency.post_delegation_mode =
        DelegationActions::from(vec![Instruction::new_with_bytes(
            system_program::id(),
            &[1],
            vec![],
        )])
        .into();
    assert_eq!(
        TestFetchCloner::clone_intent_for_request(&dependency),
        ChainlinkCloneIntent::ActionDependency
    );
}
