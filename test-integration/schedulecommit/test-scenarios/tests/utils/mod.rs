use ephemeral_rollups_sdk::consts::DELEGATION_PROGRAM_ID;
use schedulecommit_client::{
    verify::ScheduledCommitResult, ScheduleCommitTestContext,
};
use solana_sdk::{
    instruction::InstructionError, pubkey::Pubkey, signature::Signature,
    transaction::TransactionError,
};

// -----------------
// Setup
// -----------------
pub fn get_context_with_delegated_committees(
    ncommittees: usize,
) -> ScheduleCommitTestContext {
    let ctx = if std::env::var("FIXED_KP").is_ok() {
        ScheduleCommitTestContext::new(ncommittees)
    } else {
        ScheduleCommitTestContext::new_random_keys(ncommittees)
    };

    ctx.init_committees().unwrap();
    ctx.delegate_committees(None).unwrap();
    ctx
}

// -----------------
// Asserts
// -----------------
#[allow(dead_code)] // used in 02_commit_and_undelegate.rs
pub fn assert_one_committee_was_committed(
    ctx: &ScheduleCommitTestContext,
    res: &ScheduledCommitResult,
) {
    let pda = ctx.committees[0].1;

    assert_eq!(res.included.len(), 1, "includes 1 pda");
    assert_eq!(res.excluded.len(), 0, "excludes 0 pdas");

    let commit = res.included.get(&pda);
    assert!(commit.is_some(), "should have committed pda");

    assert_eq!(res.sigs.len(), 1, "should have 1 on chain sig");
}

#[allow(dead_code)] // used in 02_commit_and_undelegate.rs
pub fn assert_two_committees_were_committed(
    ctx: &ScheduleCommitTestContext,
    res: &ScheduledCommitResult,
) {
    let pda1 = ctx.committees[0].1;
    let pda2 = ctx.committees[1].1;

    assert_eq!(res.included.len(), 2, "includes 2 pdas");
    assert_eq!(res.excluded.len(), 0, "excludes 0 pdas");

    let commit1 = res.included.get(&pda1);
    let commit2 = res.included.get(&pda2);
    assert!(commit1.is_some(), "should have committed pda1");
    assert!(commit2.is_some(), "should have committed pda2");

    assert_eq!(res.sigs.len(), 1, "should have 1 on chain sig");
}

#[allow(dead_code)] // used in 02_commit_and_undelegate.rs
pub fn assert_one_committee_synchronized_count(
    ctx: &ScheduleCommitTestContext,
    res: &ScheduledCommitResult,
    expected_count: u64,
) {
    let pda = ctx.committees[0].1;

    let commit = res.included.get(&pda);
    assert!(commit.is_some(), "should have committed pda");

    assert_eq!(
        commit.unwrap().ephem_account.as_ref().unwrap().count,
        expected_count,
        "pda ({}) count is {} on ephem",
        pda,
        expected_count
    );
    assert_eq!(
        commit.unwrap().chain_account.as_ref().unwrap().count,
        expected_count,
        "pda ({}) count is {} on chain",
        pda,
        expected_count
    );
}

#[allow(dead_code)]
// used in 01_commits.rs
// used in 02_commit_and_undelegate.rs
pub fn assert_two_committees_synchronized_count(
    ctx: &ScheduleCommitTestContext,
    res: &ScheduledCommitResult,
    expected_count: u64,
) {
    let pda1 = ctx.committees[0].1;
    let pda2 = ctx.committees[1].1;

    let commit1 = res.included.get(&pda1);
    let commit2 = res.included.get(&pda2);

    assert_eq!(
        commit1.unwrap().ephem_account.as_ref().unwrap().count,
        expected_count,
        "pda1 ({}) count is {} on ephem",
        pda1,
        expected_count
    );
    assert_eq!(
        commit1.unwrap().chain_account.as_ref().unwrap().count,
        expected_count,
        "pda1 ({}) count is {} on chain",
        pda1,
        expected_count
    );
    assert_eq!(
        commit2.unwrap().ephem_account.as_ref().unwrap().count,
        expected_count,
        "pda2 ({}) count is {} on ephem",
        pda2,
        expected_count
    );
    assert_eq!(
        commit2.unwrap().chain_account.as_ref().unwrap().count,
        expected_count,
        "pda2 ({}) count is {} on chain",
        pda2,
        expected_count
    );
}

#[allow(dead_code)] // used in 02_commit_and_undelegate.rs
pub fn assert_one_committee_account_was_undelegated_on_chain(
    ctx: &ScheduleCommitTestContext,
) {
    let pda = ctx.committees[0].1;
    let id = schedulecommit_program::id();
    assert_account_was_undelegated_on_chain(ctx, pda, id);
}

#[allow(dead_code)] // used in 02_commit_and_undelegate.rs
pub fn assert_two_committee_accounts_were_undelegated_on_chain(
    ctx: &ScheduleCommitTestContext,
) {
    let pda1 = ctx.committees[0].1;
    let pda2 = ctx.committees[1].1;
    let id = schedulecommit_program::id();
    assert_account_was_undelegated_on_chain(ctx, pda1, id);
    assert_account_was_undelegated_on_chain(ctx, pda2, id);
}

#[allow(dead_code)] // used in 02_commit_and_undelegate.rs
pub fn assert_account_was_undelegated_on_chain(
    ctx: &ScheduleCommitTestContext,
    pda: Pubkey,
    new_owner: Pubkey,
) {
    let owner = ctx.fetch_chain_account_owner(pda).unwrap();
    assert_ne!(
        owner, DELEGATION_PROGRAM_ID,
        "not owned by delegation program"
    );
    assert_eq!(owner, new_owner, "new owner");
}

#[allow(dead_code)] // used in 02_commit_and_undelegate.rs
pub fn assert_tx_failed_with_instruction_error(
    tx_result: Result<Signature, solana_rpc_client_api::client_error::Error>,
    ix_error: InstructionError,
) {
    let (tx_result_err, tx_err) = extract_transaction_error(tx_result);
    let tx_err = tx_err.unwrap_or_else(|| {
        panic!("Expected TransactionError, got: {:?}", tx_result_err)
    });
    assert_is_instruction_error(tx_err, &tx_result_err, ix_error);
}

pub fn assert_is_instruction_error(
    tx_err: TransactionError,
    tx_result_err: &solana_rpc_client_api::client_error::Error,
    ix_error: InstructionError,
) {
    assert!(
        matches!(
            tx_err,
            TransactionError::InstructionError(_, err)
            if err == ix_error
        ),
        "Expected InstructionError({:?}), got: {:?}",
        ix_error,
        tx_result_err
    );
}

pub fn extract_transaction_error(
    tx_result: Result<Signature, solana_rpc_client_api::client_error::Error>,
) -> (
    solana_rpc_client_api::client_error::Error,
    Option<TransactionError>,
) {
    let tx_result_err = match tx_result {
        Ok(sig) => panic!("Expected error, got signature: {:?}", sig),
        Err(err) => err,
    };
    let tx_err = tx_result_err.get_transaction_error();
    (tx_result_err, tx_err)
}
