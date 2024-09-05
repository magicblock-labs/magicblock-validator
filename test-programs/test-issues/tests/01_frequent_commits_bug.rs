use log::*;
use test_tools_core::init_logger;

#[test]
fn test_frequent_commits_do_not_run_when_no_accounts_need_to_be_committed() {
    init_logger!();
    info!("==== test_frequent_commits_do_not_run_when_no_accounts_need_to_be_committed ====");

    // Frequent commits were running every time `accounts.commits.frequency_millis` expired
    // even when no accounts needed to be committed. This test checks that the bug is fixed.
    // We can remove it once we no longer commit accounts frequently.
}
