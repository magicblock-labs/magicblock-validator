use magicblock_ledger::Ledger;
use num_format::{Locale, ToFormattedString};
use tabular::{Row, Table};

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
        .count_transaction_succesful_status()
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
    let account_mod_data_count = ledger
        .count_account_mod_data()
        .expect("Failed to count account mod datas")
        .to_formatted_string(&Locale::en);

    let table = Table::new("{:<}  {:>}")
        .with_row(Row::new().with_cell("Column").with_cell("Count"))
        .with_row(
            Row::new()
                .with_cell("=========================")
                .with_cell("=============="),
        )
        .with_row(
            Row::new()
                .with_cell("Blockhashes")
                .with_cell(blockhash_count),
        )
        .with_row(
            Row::new()
                .with_cell("BlockTimes")
                .with_cell(block_times_count),
        )
        .with_row(
            Row::new()
                .with_cell("TransactionStatus")
                .with_cell(transaction_status_count),
        )
        .with_row(
            Row::new()
                .with_cell("Transactions")
                .with_cell(transaction_count),
        )
        .with_row(
            Row::new()
                .with_cell("Successful Transactions")
                .with_cell(successfull_transaction_status_count),
        )
        .with_row(
            Row::new()
                .with_cell("Failed Transactions")
                .with_cell(failed_transaction_status_count),
        )
        .with_row(
            Row::new()
                .with_cell("SlotSignatures")
                .with_cell(slot_signatures_count),
        )
        .with_row(
            Row::new()
                .with_cell("AccountModDatas")
                .with_cell(account_mod_data_count),
        )
        .with_row(
            Row::new()
                .with_cell("AddressSignatures")
                .with_cell(address_signatures_count),
        )
        .with_row(
            Row::new()
                .with_cell("TransactionMemos")
                .with_cell(transaction_memos_count),
        )
        .with_row(
            Row::new()
                .with_cell("PerfSamples")
                .with_cell(perf_samples_count),
        );
    println!("{}", table);
}
