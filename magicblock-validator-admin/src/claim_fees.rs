use std::time::Duration;

use dlp::instruction_builder::validator_claim_fees;
use log::{error, info};
use magicblock_config::EphemeralConfig;
use magicblock_program::validator::validator_authority;
use magicblock_rpc_client::MagicBlockRpcClientError;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig, signature::Signer,
    transaction::Transaction,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::external_config::cluster_from_remote;

pub struct ClaimFeesTask {
    pub handle: Option<JoinHandle<()>>,
    token: CancellationToken,
}

impl ClaimFeesTask {
    pub fn new() -> Self {
        Self {
            handle: None,
            token: CancellationToken::new(),
        }
    }

    pub fn start(&mut self, config: EphemeralConfig) {
        if self.handle.is_some() {
            error!("Claim fees task already started");
            return;
        }

        let token = self.token.clone();
        let handle = tokio::spawn(async move {
            info!("Starting claim fees task");
            let mut interval = tokio::time::interval(Duration::from_secs(
                config.validator.claim_fees_interval_secs,
            ));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if let Err(err) = claim_fees(config.clone()).await {
                            error!("Failed to claim fees: {:?}", err);
                        }
                    },
                    _ = token.cancelled() => break,
                }
            }
            info!("Claim fees task stopped");
        });
        self.handle = Some(handle);
    }

    pub fn stop(&mut self) {
        if let Some(handle) = self.handle.take() {
            info!("Stopping claim fees task");
            self.token.cancel();
            handle.abort();
        }
    }
}

impl Default for ClaimFeesTask {
    fn default() -> Self {
        Self::new()
    }
}

async fn claim_fees(
    config: EphemeralConfig,
) -> Result<(), MagicBlockRpcClientError> {
    info!("Claiming validator fees");

    let url = cluster_from_remote(&config.accounts.remote);
    let rpc_client = RpcClient::new_with_commitment(
        url.url().to_string(),
        CommitmentConfig::confirmed(),
    );

    let keypair_ref = &validator_authority();
    let validator = keypair_ref.pubkey();

    let ix = validator_claim_fees(validator, None);

    let latest_blockhash = rpc_client
        .get_latest_blockhash()
        .await
        .map_err(MagicBlockRpcClientError::GetLatestBlockhash)?;

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&validator),
        &[keypair_ref],
        latest_blockhash,
    );

    rpc_client
        .send_and_confirm_transaction(&tx)
        .await
        .map_err(MagicBlockRpcClientError::SendTransaction)?;

    info!("Successfully claimed validator fees");

    Ok(())
}
