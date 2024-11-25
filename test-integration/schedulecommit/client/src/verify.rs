use integration_test_tools::scheduled_commits::ScheduledCommitResult;
use program_schedulecommit::MainAccount;

use solana_sdk::signature::Signature;

use crate::ScheduleCommitTestContext;

pub fn fetch_and_verify_commit_result_from_logs(
    ctx: &ScheduleCommitTestContext,
    sig: Signature,
) -> ScheduledCommitResult<MainAccount> {
    let res = ctx.fetch_schedule_commit_result(sig).unwrap();
    res.confirm_commit_transactions_on_chain(ctx).unwrap();
    res
}
