use std::fs;
use std::path::PathBuf;

use solana_sdk::signature::Keypair;
use solana_sdk::signer::{EncodableKey, Signer};

pub(crate) fn ledger_replay_test(ledger_path: &PathBuf) {
    match fs::exists(ledger_path) {
        Ok(x) if !x => {
            panic!("Ledger path does not exist: {}", ledger_path.display())
        }
        Err(err) => {
            panic!(
                "Error looking up ledger path: {} ({})",
                ledger_path.display(),
                err
            )
        }
        _ => {}
    }

    let keypair_file = ledger_path.join("validator-keypair.json");

    if let Ok(true) = fs::exists(&keypair_file) {
        if let Ok(kp) = Keypair::read_from_file("validator-keypair.json") {
            eprintln!("Overwriting existing keypair: {}", kp.pubkey());
        }
    }

    let kp = Keypair::new();
    Keypair::write_to_file(&kp, &keypair_file)
        .expect("Failed to write keypair");

    eprintln!(
        "Wrote test validator authority (pubkey: {}) to ledger",
        kp.pubkey()
    );
    let base58 = kp.to_base58_string();
    eprintln!(
        "Add the keypair to your environment via 'export VALIDATOR_KEYPAIR={}'\n",
        base58
    );

    // Print the base58 keypair to stdout for easy copy/paste and output capture
    println!("{}", base58);
}
