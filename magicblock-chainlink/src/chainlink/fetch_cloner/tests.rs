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

#[test]
fn replay_recovery_selects_only_accounts_with_authorized_records() {
    let local = Pubkey::new_unique();
    let confined = Pubkey::new_unique();
    let foreign = Pubkey::new_unique();
    let internal = Pubkey::new_unique();
    let record_pdas = HashSet::from([
        delegation_record_pda_from_delegated_account(&local),
        delegation_record_pda_from_delegated_account(&confined),
        internal,
    ]);

    let candidates = replay_recovery_candidates(
        [local, foreign, confined, internal],
        &record_pdas,
    );

    assert_eq!(candidates, vec![local, confined]);
}

#[test]
fn replay_recovery_record_scan_is_slot_and_authority_bounded() {
    let authority = Pubkey::new_unique();
    let config = authority_record_config(authority, Some(42));

    assert_eq!(config.account_config.min_context_slot, Some(42));
    assert_eq!(config.account_config.data_slice.unwrap().length, 0);
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
