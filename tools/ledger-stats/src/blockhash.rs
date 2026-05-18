use clap::ValueEnum;
use magicblock_ledger::Ledger;
use num_format::{Locale, ToFormattedString};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum BlockhashQuery {
    Last,
}

pub(crate) fn print_blockhash_details(ledger: &Ledger, query: BlockhashQuery) {
    match query {
        BlockhashQuery::Last => match ledger.get_max_blockhash() {
            Ok((slot, hash)) => match ledger.count_blockhashes() {
                Ok(count) => {
                    println!(
                        "Last blockhash at slot {}: {} of {} total blockhashes",
                        slot.to_formatted_string(&Locale::en),
                        hash,
                        count.to_formatted_string(&Locale::en),
                    );
                }
                Err(err) => {
                    eprintln!("Failed to count blockhashes: {:?}", err);
                }
            },
            Err(err) => {
                eprintln!("Blockhash not found {:?}", err);
            }
        },
    };
}
