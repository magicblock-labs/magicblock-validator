use schedulecommit_client::{
    verify::ScheduledCommitResult, ScheduleCommitTestContext,
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
    ctx.delegate_committees().unwrap();
    ctx
}

// -----------------
// Asserts
// -----------------
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
        commit1.unwrap().ephem_account.count,
        expected_count,
        "pda1 ({}) count is {} on ephem",
        pda1,
        expected_count
    );
    assert_eq!(
        commit1.unwrap().chain_account.count,
        expected_count,
        "pda1 ({}) count is {} on chain",
        pda1,
        expected_count
    );
    assert_eq!(
        commit2.unwrap().ephem_account.count,
        expected_count,
        "pda2 ({}) count is {} on ephem",
        pda2,
        expected_count
    );
    assert_eq!(
        commit2.unwrap().chain_account.count,
        expected_count,
        "pda2 ({}) count is {} on chain",
        pda2,
        expected_count
    );
}
