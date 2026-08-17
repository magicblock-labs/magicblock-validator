use solana_account::{Account, AccountBuilder, AccountMode};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;

use super::*;

type TestFetchCloner = FetchCloner<ChainRpcClientMock, ChainPubsubClientMock>;

use crate::{
    cloner::{AccountCloneRequest, ClonePostDelegationMode, DelegationActions},
    remote_account_provider::chain_pubsub_client::mock::ChainPubsubClientMock,
    testing::rpc_client_mock::ChainRpcClientMock,
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
fn replay_recovery_request_scan_is_slot_and_type_bounded() {
    let config = undelegation_request_config(42);

    assert_eq!(config.account_config.min_context_slot, Some(42));
    assert!(matches!(
        config.filters.as_deref(),
        Some([
            RpcFilterType::DataSize(size),
            RpcFilterType::Memcmp(memcmp),
        ]) if *size == UndelegationRequest::size_with_discriminator() as u64
            && memcmp.offset() == 0
            && memcmp.bytes().is_some_and(|bytes| {
                bytes.as_slice()
                    == AccountDiscriminator::UndelegationRequest
                        .to_bytes()
                        .as_slice()
            })
    ));
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
