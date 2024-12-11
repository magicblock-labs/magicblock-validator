use magicblock_ledger::Ledger;
use std::path::PathBuf;
use std::str::FromStr;
use structopt::StructOpt;

mod counts;

#[derive(Debug, Default, StructOpt)]
enum Command {
    #[default]
    Counts,
}

impl FromStr for Command {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use Command::*;
        match s.to_lowercase().as_str() {
            "counts" => Ok(Counts),
            _ => Err(format!("Unknown format: {}", s)),
        }
    }
}

#[derive(StructOpt)]
struct Cli {
    #[structopt(subcommand)]
    command: Option<Command>,

    #[structopt(parse(from_os_str))]
    ledger_path: PathBuf,
}

fn main() {
    let args = Cli::from_args();

    let ledger =
        Ledger::open(&args.ledger_path).expect("Failed to open ledger");

    match args.command.unwrap_or_default() {
        Command::Counts => counts::print_counts(&ledger),
    }
}
