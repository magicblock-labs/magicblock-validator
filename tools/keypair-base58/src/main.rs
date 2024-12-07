use std::env;
use std::fs::File;
use std::io::Read;

fn main() {
    // Get command line argument
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: {} <keypair-file>", args[0]);
        std::process::exit(1);
    }

    // Read the keypair file
    let mut file = File::open(&args[1]).expect("Failed to open keypair file");
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .expect("Failed to read keypair file");

    // Parse the JSON array
    let keypair: Vec<u8> = serde_json::from_str(&contents).expect("Failed to parse keypair JSON");

    // Convert to base58
    let base58_string = bs58::encode(&keypair).into_string();

    // Print the result
    println!("{}", base58_string);
}
