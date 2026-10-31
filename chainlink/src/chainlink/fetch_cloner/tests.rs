use solana_account::{Account, AccountBuilder, AccountMode, ReadableAccount};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;

use super::*;

type TestFetchCloner = FetchCloner<ChainRpcClientMock, ChainPubsubClientMock>;

use crate::{
    cloner::{AccountCloneRequest, ClonePostDelegationMode, DelegationActions},
    remote_account_provider::chain_pubsub_client::mock::ChainPubsubClientMock,
    testing::{context::TestContext, rpc_client_mock::ChainRpcClientMock},
};

fn request(account: AccountBuilder) -> AccountCloneRequest {
    AccountCloneRequest {
        pubkey: Pubkey::new_unique(),
        account,
        post_delegation_mode: ClonePostDelegationMode::None,
        delegated_to_other: None,
        source_slots: None,
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
    .mode(AccountMode::Mirrored)
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
    dependency.post_delegation_mode = ClonePostDelegationMode::from(DelegationActions::new(
        Pubkey::new_unique(),
        vec![Instruction::new_with_bytes(system_program::id(), &[1], vec![])],
    ));
    assert_eq!(
        TestFetchCloner::clone_intent_for_request(&dependency),
        ChainlinkCloneIntent::ActionDependency
    );
}

/// Proves absent writable dependencies remain available for ER-only creation,
/// while missing read-only and stale local writable dependencies are fetched.
#[test]
fn post_delegation_dependency_fetch_policy() {
    assert!(!TestFetchCloner::action_dependency_needs_fetch(
        None, 10, true,
    ));
    assert!(TestFetchCloner::action_dependency_needs_fetch(
        None, 10, false,
    ));
    assert!(TestFetchCloner::action_dependency_needs_fetch(
        Some((9, AccountMode::Mirrored)),
        10,
        true,
    ));
    assert!(!TestFetchCloner::action_dependency_needs_fetch(
        Some((10, AccountMode::Mirrored)),
        10,
        true,
    ));
    assert!(!TestFetchCloner::action_dependency_needs_fetch(
        Some((9, AccountMode::Delegated)),
        10,
        true,
    ));
}

/// Proves a newer request waiting behind an older materialization re-reads the
/// bank and applies its own image instead of inheriting the older result, while
/// Engine skips an older image without changing the local account.
#[tokio::test]
async fn waiter_applies_newer_account_image() {
    let ctx = TestContext::init(11).await;
    let fetch = ctx.chainlink.fetch_cloner().expect("test Chainlink has a fetch cloner");
    let pubkey = Pubkey::new_unique();
    let build = |slot, byte| AccountCloneRequest {
        pubkey,
        account: AccountBuilder::default()
            .lamports(1_000_000)
            .data(vec![byte])
            .owner(system_program::id())
            .mode(AccountMode::Mirrored)
            .slot(slot),
        post_delegation_mode: ClonePostDelegationMode::None,
        delegated_to_other: None,
        source_slots: None,
    };
    let older = build(11, 1);
    let newer = build(12, 2);
    let accessor = ctx.bank.account(pubkey).await.unwrap();
    let older = fetch.submit_account(
        accessor,
        older,
        AccountFetchContext::rpc_get_multiple_accounts(),
    );
    let newer = fetch.clone_account(newer, AccountFetchContext::rpc_get_multiple_accounts());
    let (older, newer) = tokio::join!(older, newer);
    older.expect("older materialization succeeds");
    newer.expect("newer waiter materializes its own image");

    let state = ctx
        .bank
        .accounts()
        .loader()
        .read(&pubkey, |account| (account.slot(), account.data().to_vec()))
        .unwrap()
        .unwrap();
    assert_eq!(state, (12, vec![2]));

    let mut stale = build(10, 3);
    stale.account = stale.account.mode(AccountMode::Delegated);
    fetch
        .clone_account(stale, AccountFetchContext::rpc_get_multiple_accounts())
        .await
        .expect("stale source is skipped");
    let state = ctx
        .bank
        .accounts()
        .loader()
        .read(&pubkey, |account| {
            (account.mode(), account.slot(), account.data().to_vec())
        })
        .unwrap()
        .unwrap();
    assert_eq!(state, (AccountMode::Mirrored, 12, vec![2]));
}

/// Proves streamed completion advances the delegation slot without HTTP,
/// while older and same-slot images cannot replace protected ER state.
#[tokio::test]
async fn undelegation_completion_requires_newer_base_slot() {
    let ctx = TestContext::init(10).await;
    let pubkey = Pubkey::new_unique();
    let owner = system_program::id();
    let remote_account = |data| Account {
        lamports: 1_000_000,
        data: vec![data],
        owner,
        executable: false,
        rent_epoch: 0,
    };

    let transient = AccountBuilder::from(remote_account(7))
        .owner(dpi::id())
        .mode(AccountMode::Undelegating)
        .slot(100)
        .build();
    ctx.bank
        .accounts()
        .store(&[(pubkey, transient)])
        .expect("seed transient account");
    ctx.chainlink
        .undelegation_requested(pubkey)
        .await
        .expect("watch undelegating account");

    ctx.rpc_client.block_fetches();
    for (slot, byte, expected_mode, expected_slot, expected_byte) in [
        (99, 0, AccountMode::Undelegating, 100, 7),
        (100, 0, AccountMode::Undelegating, 100, 7),
        (101, 8, AccountMode::Mirrored, 101, 8),
        // A replay cannot roll back the completed image.
        (100, 0, AccountMode::Mirrored, 101, 8),
        (102, 9, AccountMode::Mirrored, 102, 9),
    ] {
        ctx.rpc_client.set_slot(slot);
        assert!(
            ctx.send_and_receive_account_update(pubkey, remote_account(byte), None).await,
            "update at slot {slot} should finish while HTTP fetches are blocked"
        );
        assert_eq!(
            ctx.bank
                .accounts()
                .loader()
                .read(&pubkey, |account| {
                    (account.mode(), account.slot(), account.data().to_vec())
                })
                .expect("read local account"),
            Some((expected_mode, expected_slot, vec![expected_byte])),
            "local state after update at slot {slot}"
        );
    }
    ctx.rpc_client.allow_fetches();
}

mod aml_check_strategy {
    use solana_instruction::AccountMeta;

    use super::*;

    fn action_for_program(program_id: Pubkey) -> Instruction {
        Instruction::new_with_bytes(program_id, &[], vec![])
    }

    fn action_referencing(program_id: Pubkey, account: Pubkey) -> Instruction {
        Instruction::new_with_bytes(
            program_id,
            &[],
            vec![AccountMeta::new_readonly(account, false)],
        )
    }

    #[test]
    fn all_signers_strategy_always_requires_check() {
        // An action that touches no risk-relevant program still gets checked.
        let actions = vec![action_for_program(Pubkey::new_unique())];
        assert!(delegation_actions_require_risk_check(
            AmlCheckStrategy::AllSigners,
            &actions,
        ));
    }

    #[test]
    fn relevant_programs_strategy_skips_unrelated_actions() {
        let actions = vec![
            action_for_program(Pubkey::new_unique()),
            action_for_program(Pubkey::new_unique()),
        ];
        assert!(!delegation_actions_require_risk_check(
            AmlCheckStrategy::RelevantPrograms,
            &actions,
        ));
    }

    #[test]
    fn relevant_programs_strategy_matches_each_relevant_program() {
        // Spelled out rather than iterating RISK_RELEVANT_PROGRAMS, so that
        // dropping a program from that list fails this test.
        for program in [
            TOKEN_PROGRAM_ID,
            TOKEN_2022_PROGRAM_ID,
            EATA_PROGRAM_ID,
            magic_program_api::ID,
            system_program::ID,
            magic_program_api::EPHEMERAL_SYSTEM_PROGRAM_ID,
        ] {
            let actions = vec![action_for_program(program)];
            assert!(
                delegation_actions_require_risk_check(AmlCheckStrategy::RelevantPrograms, &actions,),
                "program {program} invoked as program_id should require check",
            );

            // Referenced as a CPI target account, not the invoked program.
            let actions = vec![action_referencing(Pubkey::new_unique(), program)];
            assert!(
                delegation_actions_require_risk_check(AmlCheckStrategy::RelevantPrograms, &actions,),
                "program {program} referenced as account should require check",
            );
        }
    }

    #[test]
    fn default_strategy_checks_all_signers() {
        // The narrower strategy must be opted into: defaulting to it would
        // silently drop coverage for deployments that only set `enabled`.
        assert_eq!(AmlCheckStrategy::default(), AmlCheckStrategy::AllSigners);
    }

    #[test]
    fn relevant_programs_strategy_matches_native_sol_transfers() {
        let actions = vec![solana_system_interface::instruction::transfer(
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            1_000,
        )];
        assert!(delegation_actions_require_risk_check(
            AmlCheckStrategy::RelevantPrograms,
            &actions,
        ));
    }

    #[test]
    fn relevant_programs_strategy_matches_when_any_action_is_relevant() {
        let actions =
            vec![action_for_program(Pubkey::new_unique()), action_for_program(EATA_PROGRAM_ID)];
        assert!(delegation_actions_require_risk_check(
            AmlCheckStrategy::RelevantPrograms,
            &actions,
        ));
    }
}
