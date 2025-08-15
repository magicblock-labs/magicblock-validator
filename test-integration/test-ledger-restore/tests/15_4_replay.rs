use magicblock_config::LedgerResumeStrategy;
use test_ledger_restore::resume_strategies::test_resume_strategy;

#[test]
fn restore_ledger_replay() {
    test_resume_strategy(LedgerResumeStrategy::Replay, None);
}
