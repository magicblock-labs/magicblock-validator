use std::time::Duration;

use dlp::instruction_builder::validator_claim_fees;
use magicblock_program::validator::validator_authority;
use magicblock_rpc_client::MagicBlockRpcClientError;
use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;
use solana_transaction::Transaction;
use tokio::{task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument};

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

    pub fn start(&mut self, tick_period: Duration, url: String) {
        if self.handle.is_some() {
            error!("Claim fees task already started");
            return;
        }

        let token = self.token.clone();
        let handle = tokio::spawn(run_claim_fees_loop(token, tick_period, url));
        self.handle = Some(handle);
    }

    pub async fn stop(&mut self) {
        if let Some(handle) = self.handle.take() {
            info!("Stopping claim fees task");
            self.token.cancel();
            // Give the task a grace period to shut down gracefully
            if tokio::time::timeout(Duration::from_secs(2), handle)
                .await
                .is_err()
            {
                error!("Claim fees task did not stop within grace period");
            }
        }
    }
}

impl Default for ClaimFeesTask {
    fn default() -> Self {
        Self::new()
    }
}

#[instrument(skip(token), fields(tick_period_ms = tick_period.as_millis() as u64, url = %url))]
async fn run_claim_fees_loop(
    token: CancellationToken,
    tick_period: Duration,
    url: String,
) {
    info!("Starting claim fees task");
    let start_time = Instant::now() + tick_period;
    let mut interval = tokio::time::interval_at(start_time, tick_period);
    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(err) = claim_fees(url.clone()).await {
                    error!(error = ?err, "Failed to claim fees");
                }
            },
            _ = token.cancelled() => break,
        }
    }
    info!("Claim fees task stopped");
}

#[instrument(fields(validator = %validator_authority().pubkey()))]
async fn claim_fees(url: String) -> Result<(), MagicBlockRpcClientError> {
    info!("Claiming validator fees");

    let rpc_client =
        RpcClient::new_with_commitment(url, CommitmentConfig::confirmed());

    let keypair_ref = &validator_authority();
    let validator = keypair_ref.pubkey();

    let ix = validator_claim_fees(validator, None);

    let latest_blockhash =
        rpc_client.get_latest_blockhash().await.map_err(|e| {
            MagicBlockRpcClientError::GetLatestBlockhash(Box::new(e))
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
        .map_err(|e| MagicBlockRpcClientError::SendTransaction(Box::new(e)))?;

    info!("Successfully claimed validator fees");

    Ok(())
}
