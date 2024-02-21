mod utils;

use sleipnir_bank::bank::Bank;
use sleipnir_bank::bank_dev_utils;

#[test]
fn test_bank_one_system_instruction() {
    let mut bank = Bank::default_for_tests();
    let txs = utils::create_transactions(&bank, 1);
    eprintln!("txs: {:?}", txs);
}
