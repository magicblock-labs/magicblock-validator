use magicblock_ledger::Ledger;
use num_format::{Locale, ToFormattedString};

use crate::utils::print_two_col_table;

pub(crate) fn print_counts(ledger: &Ledger) {
    let block_times_count = ledger
        .count_block_times()
        .expect("Failed to count block times")
        .to_formatted_string(&Locale::en);
    let blockhash_count = ledger
        .count_blockhashes()
        .expect("Failed to count blockhash")
        .to_formatted_string(&Locale::en);
    let transaction_status_count = ledger
        .count_transaction_status()
        .expect("Failed to count transaction status")
        .to_formatted_string(&Locale::en);
    let successfull_transaction_status_count = ledger
        .count_transaction_successful_status()
        .expect("Failed to count successful transaction status")
        .to_formatted_string(&Locale::en);
    let failed_transaction_status_count = ledger
        .count_transaction_failed_status()
        .expect("Failed to count failed transaction status")
        .to_formatted_string(&Locale::en);
    let address_signatures_count = ledger
        .count_address_signatures()
        .expect("Failed to count address signatures")
        .to_formatted_string(&Locale::en);
    let slot_signatures_count = ledger
        .count_slot_signatures()
        .expect("Failed to count slot signatures")
        .to_formatted_string(&Locale::en);
    let transaction_count = ledger
        .count_transactions()
        .expect("Failed to count transaction")
        .to_formatted_string(&Locale::en);
    let transaction_memos_count = ledger
        .count_transaction_memos()
        .expect("Failed to count transaction memos")
        .to_formatted_string(&Locale::en);
    let perf_samples_count = ledger
        .count_perf_samples()
        .expect("Failed to count perf samples")
        .to_formatted_string(&Locale::en);

    let rows = vec![
        ("Blockhashes".to_string(), blockhash_count),
        ("BlockTimes".to_string(), block_times_count),
        ("TransactionStatus".to_string(), transaction_status_count),
        ("Transactions".to_string(), transaction_count),
        (
            "Successful Transactions".to_string(),
            successfull_transaction_status_count,
        ),
        (
            "Failed Transactions".to_string(),
            failed_transaction_status_count,
        ),
        ("SlotSignatures".to_string(), slot_signatures_count),
        ("AddressSignatures".to_string(), address_signatures_count),
        ("TransactionMemos".to_string(), transaction_memos_count),
        ("PerfSamples".to_string(), perf_samples_count),
    ];
    print_two_col_table(None, ["Column", "Count"], &rows);
}
