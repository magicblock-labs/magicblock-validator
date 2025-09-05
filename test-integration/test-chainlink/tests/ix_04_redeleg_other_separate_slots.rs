// Implements the following flow:
//
// ## Redelegate an Account that was delegated to us to Other - Separate Slots
// @docs/flows/deleg-us-redeleg-other.md
//
// NOTE: This scenario requires delegating to an arbitrary "other" authority on-chain,
// which is not yet supported by our integration harness. We add the test skeleton
// and mark it ignored until the necessary on-chain instruction is available.

use magicblock_chainlink::{skip_if_no_test_validator, testing::init_logger};

use test_chainlink::ixtest_context::IxtestContext;

#[tokio::test]
#[ignore = "blocked: cannot delegate to arbitrary authority in ix env yet"]
async fn ixtest_undelegate_redelegate_to_other_in_separate_slot() {
    init_logger();
    skip_if_no_test_validator!();

    let _ctx = IxtestContext::init().await;

    // TODO(thlorenz): @ Implement once we can delegate to a specific authority in integration tests.
}
