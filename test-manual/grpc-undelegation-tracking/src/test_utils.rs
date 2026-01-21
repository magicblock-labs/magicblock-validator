use anyhow::{Context, Result};
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    instruction::Instruction,
    message::Message,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::Transaction,
};

/// Keypair bytes for test validator mAGicPQYBMvcYveUZA5F5UNNwyHvfYh5xkLS2Fr1mev
const VALIDATOR_KEYPAIR_BYTES: [u8; 64] = [
    7, 83, 184, 55, 200, 223, 238, 137, 166, 244, 107, 126, 189, 16, 194, 36,
    228, 68, 43, 143, 13, 91, 3, 81, 53, 253, 26, 36, 50, 198, 40, 159, 11, 80,
    9, 208, 183, 189, 108, 200, 89, 77, 168, 76, 233, 197, 132, 22, 21, 186,
    202, 240, 105, 168, 157, 64, 233, 249, 100, 104, 210, 41, 83, 87,
];

/// Load payer keypair from KEYPAIR_PATH env and create validator keypair
pub fn load_keypairs() -> Result<(Keypair, Keypair)> {
    let keypair_path = std::env::var("KEYPAIR_PATH")
        .context("KEYPAIR_PATH environment variable not set")?;

    let payer = solana_sdk::signature::read_keypair_file(&keypair_path)
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to read keypair from {}: {}",
                keypair_path,
                e
            )
        })?;

    let validator = Keypair::from_bytes(&VALIDATOR_KEYPAIR_BYTES)
        .context("Failed to create validator keypair from bytes")?;

    Ok((payer, validator))
}

/// Create RPC clients for devnet (via Helius) and ephemeral validator
pub fn create_rpc_clients() -> Result<(RpcClient, RpcClient)> {
    let helius_api_key = std::env::var("HELIUS_API_KEY")
        .context("HELIUS_API_KEY environment variable not set")?;

    let devnet_url =
        format!("https://devnet.helius-rpc.com/?api-key={}", helius_api_key);
    let ephemeral_url = "http://127.0.0.1:9988";

    let devnet_rpc = RpcClient::new_with_commitment(
        devnet_url,
        CommitmentConfig::confirmed(),
    );
    let ephemeral_rpc = RpcClient::new_with_commitment(
        ephemeral_url,
        CommitmentConfig::confirmed(),
    );

    Ok((devnet_rpc, ephemeral_rpc))
}

/// Send and confirm a transaction with the given instructions
pub fn send_and_confirm(
    rpc: &RpcClient,
    instructions: &[Instruction],
    payer: &Keypair,
    signers: &[&Keypair],
) -> Result<Signature> {
    let recent_blockhash = rpc
        .get_latest_blockhash()
        .context("Failed to get latest blockhash")?;

    let message = Message::new(instructions, Some(&payer.pubkey()));
    let mut transaction = Transaction::new_unsigned(message);

    let mut all_signers: Vec<&Keypair> = vec![payer];
    all_signers.extend(signers);

    transaction
        .try_sign(&all_signers, recent_blockhash)
        .context("Failed to sign transaction")?;

    let config = RpcSendTransactionConfig {
        skip_preflight: true,
        ..Default::default()
    };

    // Retry loop to handle transient blockhash issues with devnet
    let mut last_error = None;
    for attempt in 1..=3 {
        match rpc.send_and_confirm_transaction_with_spinner_and_config(
            &transaction,
            rpc.commitment(),
            config,
        ) {
            Ok(signature) => return Ok(signature),
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("Blockhash not found") && attempt < 3 {
                    tracing::warn!(
                        "Blockhash expired (attempt {}), retrying with fresh blockhash...",
                        attempt
                    );
                    let new_blockhash = rpc
                        .get_latest_blockhash()
                        .context("Failed to get latest blockhash on retry")?;
                    transaction
                        .try_sign(&all_signers, new_blockhash)
                        .context("Failed to re-sign transaction")?;
                    last_error = Some(e);
                    continue;
                }
                return Err(e).context("Failed to send and confirm transaction");
            }
        }
    }

    Err(last_error.unwrap()).context("Failed to send and confirm transaction")
}
