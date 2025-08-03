use dlp::instruction_builder::validator_claim_fees;
use magicblock_api::errors::{ApiError, ApiResult};
use magicblock_config::EphemeralConfig;
use magicblock_program::validator::validator_authority;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::{CommitmentConfig, CommitmentLevel},
    signature::Signer,
    transaction::Transaction,
};

use crate::external_config::cluster_from_remote;

pub async fn claim_fees(config: EphemeralConfig) -> ApiResult<()> {
    info!("Claiming validator fees");

    // building the config
    let url = cluster_from_remote(&config.accounts.remote);
    let rpc_client = RpcClient::new_with_commitment(
        url.url().to_string(),
        CommitmentConfig::confirmed(),
    );

    // setting the keypair and validator pubkey
    let keypair_ref = &validator_authority();
    let validator = keypair_ref.pubkey();

    let ix = validator_claim_fees(validator, None);

    let latest_blockhash =
        rpc_client.get_latest_blockhash().await.map_err(|err| {
            ApiError::FailedToGetBlockhash(format!(
                "Failed to get blockhash: {:?}",
                err
            ))
        })?;

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&validator),
        &[keypair_ref],
        latest_blockhash,
    );

    rpc_client
        .send_and_confirm_transaction(&tx)
        .await
        .map_err(|err| {
            ApiError::FailedToSendTransaction(format!(
                "Failed to send transaction: {:?}",
                err
            ))
        })?;

    info!("Successfully claimed validator fees");

    Ok(())
}
