use std::{
    fs,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use solana_sdk::{commitment_config::CommitmentConfig, pubkey::Pubkey};
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
    let accounts_dir = temp_dir_path.join("accounts");
    fs::create_dir(&accounts_dir).expect("Failed to create accounts directory");

    let file_path = temp_dir_path.join("run-validator.sh");
    let accounts: Vec<Pubkey> = if let Some(ledger_path) = ledger_path {
        let adb =
            AccountsDb::open(ledger_path).expect("failed to open accountsdb");
        eprintln!(
            "Generating test validator script with accounts from accountsdb: {:?}",
            ledger_path
        );
        adb.iter_all().map(|(pk, _)| pk).collect()
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

    download_accounts_into_from_url_into_dir(
        &accounts,
        config.url.clone(),
        &accounts_dir,
    );

    args.push("--account-dir".into());
    args.push(accounts_dir.to_string_lossy().to_string());

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
    // Set permissions
    #[cfg(unix)]
    {
        fs::set_permissions(&file_path, fs::Permissions::from_mode(0o755))
            .expect("Failed to set permissions on Unix");
    }

    #[cfg(windows)]
    {
        use std::process::Command;

        let output = Command::new("icacls")
            .arg(&file_path)
            .arg("/grant")
            .arg("Everyone:(RX)")
            .arg("/grant")
            .arg("Users:(RX)")
            .arg("/grant")
            .arg("Administrators:(F)")
            .output()
            .expect("Failed to set file permissions on Windows");

        if !output.status.success() {
            eprintln!("Error: {:?}", String::from_utf8_lossy(&output.stderr));
        } else {
            println!("Permissions set successfully on Windows!");
        }
    }

    eprintln!(
        "Run this script to start the test validator: \n\n{}",
        file_path.display()
    );
}

fn download_accounts_into_from_url_into_dir(
    pubkeys: &[Pubkey],
    url: String,
    dir: &Path,
) {
    // Derived from error from helius RPC: Failed to download accounts: Error { request: Some(GetMultipleAccounts), kind: RpcError(RpcResponseError { code: -32602, message: "Too many inputs provided; max 100", data: Empty }) }
    const MAX_ACCOUNTS: usize = 100;
    let rpc_client =
        RpcClient::new_with_commitment(url, CommitmentConfig::confirmed());
    let total_len = pubkeys.len();
    for (idx, pubkeys) in pubkeys.chunks(MAX_ACCOUNTS).enumerate() {
        let start = idx * MAX_ACCOUNTS;
        let end = start + pubkeys.len();
        eprintln!("Downloading {}..{}/{} accounts", start, end, total_len);
        match rpc_client.get_multiple_accounts(pubkeys) {
            Ok(accs) => accs
                .into_iter()
                .zip(pubkeys)
                .filter_map(|(acc, pubkey)| acc.map(|x| (x, pubkey)))
                .for_each(|(acc, pubkey)| {
                    let path = dir.join(format!("{pubkey}.json"));
                    let pk = pubkey.to_string();
                    let lamports = acc.lamports;
                    let data = [
                        #[allow(deprecated)] // this is just a dev tool
                        base64::encode(&acc.data),
                        "base64".to_string(),
                    ];
                    let owner = acc.owner.to_string();
                    let executable = acc.executable;
                    let rent_epoch = acc.rent_epoch;
                    let space = acc.data.len();
                    let json: Value = json! {{
                        "pubkey": pk,
                        "account": {
                            "lamports": lamports,
                            "data": data,
                            "owner": owner,
                            "executable": executable,
                            "space": space,
                            "rentEpoch": rent_epoch
                        },
                    }};
                    fs::write(&path, format!("{:#}", json))
                        .expect("Failed to write account");
                }),
            Err(err) => eprintln!("Failed to download accounts: {:?}", err),
        }
    }
}
