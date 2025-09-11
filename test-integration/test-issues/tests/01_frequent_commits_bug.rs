use integration_test_tools::IntegrationTestContext;
use log::*;
use test_kit::init_logger;

#[test]
fn test_frequent_commits_do_not_run_when_no_accounts_need_to_be_committed() {
    // Frequent commits were running every time `accounts.commits.frequency_millis` expired
    // even when no accounts needed to be committed. This test checks that the bug is fixed.
    // We can remove it once we no longer commit accounts frequently.
    init_logger!();
    info!("==== test_frequent_commits_do_not_run_when_no_accounts_need_to_be_committed ====");

    let ctx = IntegrationTestContext::try_new().unwrap();
    let chain_client = &ctx.try_chain_client().unwrap();

    // The commits happen frequently via the MagicBlock System program.
    // Thus here we ensure that after the frequency timeout we did not receive any transaction
    // on chain. This test did fail when I uncommented the fix,
    // see (magicblock-accounts/src/external_accounts_manager.rs:commit_delegated).

    // 1. Make sure we have no transaction yet on chain
    assert_eq!(chain_client.get_transaction_count().unwrap(), 0);

    // 2. Wait for 3 more slots
    let current_slot = chain_client.get_slot().unwrap();
    while chain_client.get_slot().unwrap() < current_slot + 3 {
        std::thread::sleep(std::time::Duration::from_millis(40));
    }

    // 3. Make sure we still have no transaction on chain
    assert_eq!(chain_client.get_transaction_count().unwrap(), 0);
}
