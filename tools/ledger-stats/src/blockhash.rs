use std::str::FromStr;

use magicblock_ledger::Ledger;
use num_format::{Locale, ToFormattedString};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub(crate) enum BlockhashQuery {
    #[structopt(
        help = "Prints the latest slot and blockhash for which a blockhash was recorded"
    )]
    Last,
}

impl FromStr for BlockhashQuery {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "last" => Ok(BlockhashQuery::Last),
            _ => Err("Invalid blockhash query".to_string()),
        }
    }
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
