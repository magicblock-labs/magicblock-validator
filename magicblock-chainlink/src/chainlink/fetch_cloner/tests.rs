use solana_account::{Account, AccountBuilder, AccountMode};
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

fn request(account: AccountBuilder) -> AccountCloneRequest {
    AccountCloneRequest {
        pubkey: Pubkey::new_unique(),
        account,
        commit_frequency_ms: None,
        delegation_actions: DelegationActions::default(),
        delegated_to_other: None,
        needs_undelegation: false,
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
        let actions: DelegationActions =
            vec![action_for_program(Pubkey::new_unique())].into();
        assert!(delegation_actions_require_risk_check(
            AmlCheckStrategy::AllSigners,
            &actions,
        ));
    }

    #[test]
    fn relevant_programs_strategy_skips_unrelated_actions() {
        let actions: DelegationActions = vec![
            action_for_program(Pubkey::new_unique()),
            action_for_program(Pubkey::new_unique()),
        ]
        .into();
        assert!(!delegation_actions_require_risk_check(
            AmlCheckStrategy::RelevantPrograms,
            &actions,
        ));
    }

    #[test]
    fn relevant_programs_strategy_matches_each_relevant_program() {
        for program in [
            TOKEN_PROGRAM_ID,
            TOKEN_2022_PROGRAM_ID,
            EATA_PROGRAM_ID,
            magicblock_magic_program_api::ID,
        ] {
            let actions: DelegationActions =
                vec![action_for_program(program)].into();
            assert!(
                delegation_actions_require_risk_check(
                    AmlCheckStrategy::RelevantPrograms,
                    &actions,
                ),
                "program {program} invoked as program_id should require check",
            );

            // Referenced as a CPI target account, not the invoked program.
            let actions: DelegationActions =
                vec![action_referencing(Pubkey::new_unique(), program)].into();
            assert!(
                delegation_actions_require_risk_check(
                    AmlCheckStrategy::RelevantPrograms,
                    &actions,
                ),
                "program {program} referenced as account should require check",
            );
        }
    }

    #[test]
    fn relevant_programs_strategy_matches_when_any_action_is_relevant() {
        let actions: DelegationActions = vec![
            action_for_program(Pubkey::new_unique()),
            action_for_program(EATA_PROGRAM_ID),
        ]
        .into();
        assert!(delegation_actions_require_risk_check(
            AmlCheckStrategy::RelevantPrograms,
            &actions,
        ));
    }
}
