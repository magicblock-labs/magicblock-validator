use magicblock_ledger::Ledger;
use std::path::PathBuf;
use structopt::StructOpt;
use tabular::{Row, Table};

#[derive(StructOpt)]
struct Cli {
    /// The path to the ledger
    #[structopt(parse(from_os_str))]
    ledger_path: PathBuf,

    /// The format to display the output
    #[structopt(long, default_value = "counts")]
    format: String,
}

fn main() {
    let args = Cli::from_args();

    let ledger = Ledger::open(&args.ledger_path).expect("Failed to open ledger");

    let block_times_count = ledger
        .count_block_times()
        .expect("Failed to count block times");

    let mut table = Table::new("{:<}  {:<}");
    table.add_row(Row::new().with_cell("Column").with_cell("Count"));
    table.add_row(
        Row::new()
            .with_cell("BlockTimes")
            .with_cell(block_times_count),
    );
    println!("{}", table);
}
