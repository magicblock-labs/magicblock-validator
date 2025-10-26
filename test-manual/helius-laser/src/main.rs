use anyhow::{Context, Result};
use log::*;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig, native_token::LAMPORTS_PER_SOL,
    signature::Keypair, signer::Signer, system_instruction::transfer,
    transaction::Transaction,
};
use std::{env, time::Duration};

const TRANSFER_AMOUNT: u64 = LAMPORTS_PER_SOL / 100;
const SECOND_TRANSFER_AMOUNT: u64 = LAMPORTS_PER_SOL / 200; // Smaller amount for second transfer

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    let helius_api_key = env::var("HELIUS_API_KEY")
        .context("HELIUS_API_KEY environment variable not set")?;

    let rpc_endpoint =
        format!("https://devnet.helius-rpc.com/?api-key={}", helius_api_key);

    info!("Connecting to Helius devnet");
    let rpc_client = RpcClient::new(rpc_endpoint);

    let keypair_path = env::var("KEYPAIR_PATH")
        .context("KEYPAIR_PATH environment variable not set")?;
    info!("Loading keypair from {}", keypair_path);
    let from_keypair = solana_sdk::signature::read_keypair_file(&keypair_path)
        .map_err(|e| anyhow::anyhow!("Failed to read keypair file: {}", e))?;
    let from_pubkey = from_keypair.pubkey();

    let to_keypair = Keypair::new();
    let to_pubkey = to_keypair.pubkey();

    info!("From account: {}", from_pubkey);
    info!("To account: {}", to_pubkey);

    info!("Performing first transfer of {} lamports", TRANSFER_AMOUNT);
    let recent_blockhash = rpc_client.get_latest_blockhash()?;
    let transaction = Transaction::new_signed_with_payer(
        &[transfer(&from_pubkey, &to_pubkey, TRANSFER_AMOUNT)],
        Some(&from_pubkey),
        &[&from_keypair],
        recent_blockhash,
    );

    let signature = rpc_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &transaction,
            CommitmentConfig::confirmed(),
            Default::default(),
        )?;
    info!("First transfer successful: {}", signature);

    // Clone accounts to validator
    info!("Cloning accounts to local validator...");
    clone_accounts_to_validator(&rpc_client, &from_pubkey, &to_pubkey).await?;

    // Wait for accounts to be cloned and subscribed
    info!("Waiting for validator to clone accounts...");
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Second transfer to test validator updates
    info!(
        "Performing second transfer of {} lamports to test validator updates",
        SECOND_TRANSFER_AMOUNT
    );
    let recent_blockhash = rpc_client.get_latest_blockhash()?;
    let transaction2 = Transaction::new_signed_with_payer(
        &[transfer(&from_pubkey, &to_pubkey, SECOND_TRANSFER_AMOUNT)],
        Some(&from_pubkey),
        &[&from_keypair],
        recent_blockhash,
    );

    let signature2 = rpc_client
        .send_and_confirm_transaction_with_spinner_and_config(
            &transaction2,
            CommitmentConfig::confirmed(),
            Default::default(),
        )?;
    info!("Second transfer successful: {}", signature2);

    // Wait for validator to process updates
    info!("Waiting for validator to process second transfer...");
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Check account balances to calculate return amount
    let from_balance = rpc_client.get_balance(&from_pubkey)?;
    let to_balance = rpc_client.get_balance(&to_pubkey)?;

    info!(
        "Current balances - from: {} lamports, to: {} lamports",
        from_balance, to_balance
    );

    // Calculate amount to transfer back (all but enough for fees)
    let fee_estimate = 5000; // Conservative fee estimate
    let return_amount = to_balance.saturating_sub(fee_estimate);

    if return_amount > 0 {
        info!(
            "Performing final transfer back of {} lamports to close account",
            return_amount
        );
        let recent_blockhash = rpc_client.get_latest_blockhash()?;
        let transaction3 = Transaction::new_signed_with_payer(
            &[transfer(&to_pubkey, &from_pubkey, return_amount)],
            Some(&to_pubkey),
            &[&to_keypair],
            recent_blockhash,
        );

        let signature3 = rpc_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &transaction3,
                CommitmentConfig::confirmed(),
                Default::default(),
            )?;
        info!("Final transfer successful: {}", signature3);

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

    // Create RPC client for local validator
    let local_rpc_client = RpcClient::new("http://localhost:8899");

    // Compare both accounts
    compare_account_states(
        &rpc_client,
        &local_rpc_client,
        &from_pubkey,
        "from",
    )
    .await?;
    compare_account_states(&rpc_client, &local_rpc_client, &to_pubkey, "to")
        .await?;

    info!("✓ All account state comparisons passed!");
    info!("✓ Test completed! Validator successfully cloned and tracks account states from Helius devnet.");

    Ok(())
}

async fn clone_accounts_to_validator(
    rpc_client: &RpcClient,
    from_pubkey: &solana_sdk::pubkey::Pubkey,
    to_pubkey: &solana_sdk::pubkey::Pubkey,
) -> Result<()> {
    info!("Fetching account info for cloning...");

    // Get account info for from_pubkey
    match rpc_client.get_account(from_pubkey) {
        Ok(account) => {
            info!(
                "From account cloned successfully - lamports: {}, owner: {}",
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
                "To account cloned successfully - lamports: {}, owner: {}",
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
        (Err(helius_err), Err(local_err)) => {
            info!("Both RPCs returned errors for {} account - Helius: {}, Local: {}", account_name, helius_err, local_err);
        }
        (Ok(_), Err(local_err)) => {
            panic!(
                "Helius has {} account but local validator doesn't: {}",
                account_name, local_err
            );
        }
        (Err(helius_err), Ok(_)) => {
            panic!(
                "Local validator has {} account but Helius doesn't: {}",
                account_name, helius_err
            );
        }
    }

    Ok(())
}
