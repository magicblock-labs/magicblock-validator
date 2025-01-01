use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

use ledger_stats::{accounts_storage_from_ledger, open_ledger};
use solana_sdk::account::ReadableAccount;
use tempfile::tempdir;

pub struct TestValidatorConfig {
    pub rpc_port: u16,
    pub url: String,
}

pub(crate) fn gen_test_validator_start_script(
    ledger_path: Option<&PathBuf>,
    config: TestValidatorConfig,
) {
    let temp_dir = tempdir().expect("Failed to create temporary directory");
    let temp_dir_path = temp_dir.into_path();
    let file_path = temp_dir_path.join("run-validator.sh");
    let accounts = if let Some(ledger_path) = ledger_path {
        let ledger = open_ledger(ledger_path);
        eprintln!(
            "Generating test validator script with accounts from ledger: {:?}",
            ledger_path
        );
        let (storage, _) = accounts_storage_from_ledger(&ledger);
        storage
            .all_accounts()
            .into_iter()
            .map(|x| (*x.pubkey(), x.executable(), *x.owner()))
            .collect::<Vec<_>>()
    } else {
        eprintln!("Generating test validator script without accounts");
        vec![]
    };

    let mut args = vec![
        "--log".to_string(),
        "--rpc-port".to_string(),
        config.rpc_port.to_string(),
        "-r".to_string(),
        "--limit-ledger-size".to_string(),
        "10000".to_string(),
    ];

    for (pubkey, executable, owner) in accounts {
        // NOTE: we may need to treat executables differently if just cloning
        // at startup is not sufficient even though we also will clone the
        // executable data account.
        // For now we don't run programs in the test validator, but just
        // want to make sure they can be cloned by the ephemeral, so it is not
        // yet important.
        args.push("--maybe-clone".to_string());
        args.push(pubkey.to_string());
    }

    args.push("--url".into());
    args.push(config.url);

    let script = format!(
        "
#!/usr/bin/env bash
set -e
solana-test-validator  \\\n  {}",
        args.join(" \\\n  ")
    );
    // chmod u+x
    fs::write(&file_path, script)
        .expect("Failed to write test validator script");
    fs::set_permissions(&file_path, fs::Permissions::from_mode(0o755))
        .expect("Failed to set test validator script permissions");

    eprintln!(
        "Run this script to start the test validator: \n\n{}",
        file_path.display()
    );
}
