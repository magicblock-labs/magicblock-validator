use std::{env, time::Duration};

use anyhow::{Context, Result};
use log::*;
use solana_client::rpc_config::{
    RpcSendTransactionConfig, RpcTransactionConfig,
};
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    account::Account,
    commitment_config::CommitmentConfig,
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair},
    signer::Signer,
    system_instruction::transfer,
    transaction::Transaction,
};

const TRANSFER_AMOUNT: u64 = LAMPORTS_PER_SOL / 1_000;
const SECOND_TRANSFER_AMOUNT: u64 = LAMPORTS_PER_SOL / 2_000;

fn get_keypairs() -> Result<(Keypair, Keypair)> {
    let keypair_path = env::var("KEYPAIR_PATH")
        .context("KEYPAIR_PATH environment variable not set")?;
    info!("Loading keypair from {}", keypair_path);
    let from_keypair = read_keypair_file(&keypair_path)
        .map_err(|e| anyhow::anyhow!("Failed to read keypair file: {}", e))?;

    let to_keypair = Keypair::new();
    Ok((from_keypair, to_keypair))
}

fn perform_transfer(
    rpc_client: &RpcClient,
    from_keypair: &Keypair,
    to_pubkey: &Pubkey,
    amount: u64,
) -> Result<solana_sdk::signature::Signature> {
    let from_pubkey = from_keypair.pubkey();
    let (recent_blockhash, _) = rpc_client
        .get_latest_blockhash_with_commitment(rpc_client.commitment())?;
    let transaction = Transaction::new_signed_with_payer(
        &[transfer(&from_pubkey, to_pubkey, amount)],
        Some(&from_pubkey),
        &[from_keypair],
        recent_blockhash,
    );

    let signature = rpc_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &transaction,
            rpc_client.commitment(),
            RpcSendTransactionConfig {
                skip_preflight: false,
                preflight_commitment: Some(rpc_client.commitment().commitment),
                ..Default::default()
            },
        )?;
    Ok(signature)
}

fn check_balances(
    rpc_client: &RpcClient,
    local_rpc_client: &RpcClient,
    from_pubkey: &Pubkey,
    to_pubkey: &Pubkey,
) -> Result<(u64, u64, u64, u64)> {
    let from_balance = rpc_client.get_balance(&from_pubkey)?;
    let to_balance = rpc_client.get_balance(&to_pubkey)?;
    let local_from_balance = local_rpc_client.get_balance(&from_pubkey)?;
    let local_to_balance = local_rpc_client.get_balance(&to_pubkey)?;

    info!(
        "Current balances:
remote from: {} lamports, to: {} lamports
local  from: {} lamports, to: {} lamports",
        from_balance, to_balance, local_from_balance, local_to_balance
    );

    assert_eq!(
        from_balance, local_from_balance,
        "From account balances should match between remote and local"
    );
    assert_eq!(
        to_balance, local_to_balance,
        "To account balances should match between remote and local"
    );

    Ok((
        from_balance,
        local_from_balance,
        to_balance,
        local_to_balance,
    ))
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    let helius_api_key = env::var("HELIUS_API_KEY")
        .context("HELIUS_API_KEY environment variable not set")?;

    let rpc_endpoint =
        format!("https://devnet.helius-rpc.com/?api-key={}", helius_api_key);

    info!("Connecting to Helius devnet and localhost:9988");
    let remote_rpc_client = RpcClient::new_with_commitment(
        rpc_endpoint,
        CommitmentConfig::confirmed(),
    );
    let local_rpc_client = RpcClient::new_with_commitment(
        "http://127.0.0.1:9988",
        CommitmentConfig::confirmed(),
    );

    let (from_keypair, to_keypair) = get_keypairs()?;
    let from_pubkey = from_keypair.pubkey();
    let to_pubkey = to_keypair.pubkey();

    info!("From account: {}", from_pubkey);
    info!("To account:   {}", to_pubkey);

    // 1. Transfer to init the to account on devnet
    info!("Performing first transfer of {} lamports", TRANSFER_AMOUNT);
    let sig = perform_transfer(
        &remote_rpc_client,
        &from_keypair,
        &to_pubkey,
        TRANSFER_AMOUNT,
    )?;
    info!("First transfer successful: {}", sig);

    info!("Fetching accounts from local validator...");
    request_account_infos(&local_rpc_client, &from_pubkey, &to_pubkey)?;

    check_balances(
        &remote_rpc_client,
        &local_rpc_client,
        &from_pubkey,
        &to_pubkey,
    )?;

    // 2. Transfer again to test validator updates
    info!(
        "Performing second transfer of {} lamports to test validator updates",
        SECOND_TRANSFER_AMOUNT
    );
    let sig = perform_transfer(
        &remote_rpc_client,
        &from_keypair,
        &to_pubkey,
        SECOND_TRANSFER_AMOUNT,
    )?;
    info!("Second transfer successful: {}", sig);

    let (_from_balance, _local_from_balance, to_balance, _local_to_balance) =
        check_balances(
            &remote_rpc_client,
            &local_rpc_client,
            &from_pubkey,
            &to_pubkey,
        )?;

    // 3. Final transfer back to from account to close to account and check that
    //    we get the closed account update
    let tx = remote_rpc_client.get_transaction_with_config(
        &sig,
        RpcTransactionConfig {
            commitment: Some(CommitmentConfig::confirmed()),
            ..Default::default()
        },
    )?;
    let fee = tx.transaction.meta.unwrap().fee;
    let return_amount = to_balance.saturating_sub(fee);

    if return_amount > 0 {
        info!(
            "Performing final transfer back of {} lamports to close account assuming fee from last tx: {} lamports",
            return_amount,
            fee

        );
        let sig = perform_transfer(
            &remote_rpc_client,
            &to_keypair,
            &from_pubkey,
            return_amount,
        )?;
        info!("Final transfer successful: {}", sig);

        // Wait for final update
        info!("Waiting for final account update...");
        tokio::time::sleep(Duration::from_secs(3)).await;
    } else {
        info!("Skipping final transfer - insufficient balance for fees");
    }

    // Compare account states between Helius devnet and local validator
    info!(
        "Comparing account states between Helius devnet and local validator..."
    );

    // Compare both accounts
    compare_account_states(
        &remote_rpc_client,
        &local_rpc_client,
        &from_pubkey,
        "from",
    )
    .await?;
    compare_account_states(
        &remote_rpc_client,
        &local_rpc_client,
        &to_pubkey,
        "to",
    )
    .await?;

    info!("✓ All account state comparisons passed!");
    info!("✓ Test completed! Validator successfully cloned and tracks account states from Helius devnet.");

    Ok(())
}

fn request_account_infos(
    rpc_client: &RpcClient,
    from_pubkey: &solana_sdk::pubkey::Pubkey,
    to_pubkey: &solana_sdk::pubkey::Pubkey,
) -> Result<()> {
    info!("Fetching account infos...");

    // Get account info for from_pubkey
    match rpc_client.get_account(from_pubkey) {
        Ok(account) => {
            info!(
                "From account fetched successfully - lamports: {}, owner: {}",
                account.lamports, account.owner
            );
        }
        Err(e) => {
            warn!("Failed to get from account info: {}", e);
        }
    }

    // Get account info for to_pubkey
    match rpc_client.get_account(to_pubkey) {
        Ok(account) => {
            info!(
                "To account fetched successfully - lamports: {}, owner: {}",
                account.lamports, account.owner
            );
        }
        Err(e) => {
            warn!("Failed to get to account info: {}", e);
        }
    }

    Ok(())
}

async fn compare_account_states(
    helius_client: &RpcClient,
    local_client: &RpcClient,
    pubkey: &solana_sdk::pubkey::Pubkey,
    account_name: &str,
) -> Result<()> {
    // Get account states from both RPCs
    let helius_account = helius_client.get_account(pubkey);
    let local_account = local_client.get_account(pubkey);

    // Compare account states
    match (helius_account, local_account) {
        (Ok(helius), Ok(local)) => {
            assert_eq!(helius, local, "{} account state should match between Helius and local validator", account_name);
            info!("✓ {} account states match", account_name);
        }
        (Err(_helius_err), Ok(local)) => {
            // Our validator keeps empty accounts until they are evicted
            let helius = Account {
                rent_epoch: local.rent_epoch,
                ..Default::default()
            };
            assert_eq!(helius, local, "{} account state should match between Helius and local validator", account_name);
            info!("✓ {} account states match", account_name);
        }
        (Err(helius_err), Err(local_err)) => {
            info!("Both RPCs returned errors for {} account - Helius: {}, Local: {}", account_name, helius_err, local_err);
        }
        (Ok(_), Err(local_err)) => {
            panic!(
                "Helius has {} account but local validator doesn't: {}",
                account_name, local_err
            );
        }
    }

    Ok(())
}
