use solana_account::{Account, AccountMode, AccountSharedData};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;

use super::*;

type TestFetchCloner = FetchCloner<ChainRpcClientMock, ChainPubsubClientMock>;

use crate::{
    cloner::{AccountCloneRequest, DelegationActions},
    remote_account_provider::chain_pubsub_client::mock::ChainPubsubClientMock,
    testing::rpc_client_mock::ChainRpcClientMock,
};

fn request(account: AccountSharedData) -> AccountCloneRequest {
    AccountCloneRequest {
        pubkey: Pubkey::new_unique(),
        account,
        commit_frequency_ms: None,
        delegation_actions: DelegationActions::default(),
        delegated_to_other: None,
        needs_undelegation: false,
    }
}

fn account() -> AccountSharedData {
    AccountSharedData::from(Account {
        lamports: 1_000_000,
        data: vec![1, 2, 3, 4],
        owner: system_program::id(),
        executable: false,
        rent_epoch: 0,
    })
}

#[test]
fn clone_request_classification() {
    let empty = request(AccountSharedData::default());
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

    let mut delegated_account = account();
    delegated_account.set_mode(AccountMode::Delegated);
    let delegated = request(delegated_account);
    assert_eq!(
        TestFetchCloner::clone_intent_for_request(&delegated),
        ChainlinkCloneIntent::DelegationRecord
    );

    let mut dependency = request(account());
    dependency.delegation_actions =
        DelegationActions::from(vec![Instruction::new_with_bytes(
            system_program::id(),
            &[1],
            vec![],
        )]);
    assert_eq!(
        TestFetchCloner::clone_intent_for_request(&dependency),
        ChainlinkCloneIntent::ActionDependency
    );
}
