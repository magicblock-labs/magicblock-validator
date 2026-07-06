use std::time::Duration;

use dlp_api::instruction_builder::validator_claim_fees;
use engine::Engine;
use magicblock_rpc_client::MagicBlockRpcClientError;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;
use solana_transaction::Transaction;
use tokio::{task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument};

const MIN_CLAIMABLE_LAMPORTS: u64 = 100_000_000;

#[derive(Debug, thiserror::Error)]
pub enum ClaimFeesError {
    #[error(
        "Engine authority {authority} cannot be signed by local identity {signer}"
    )]
    AuthoritySignerMismatch { authority: Pubkey, signer: Pubkey },

    #[error(transparent)]
    Rpc(#[from] MagicBlockRpcClientError),
}

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

    pub fn start(
        &mut self,
        engine: Engine,
        tick_period: Duration,
        url: String,
    ) {
        if self.handle.is_some() {
            error!("Claim fees task already started");
            return;
        }

        let token = self.token.clone();
        let handle =
            tokio::spawn(run_claim_fees_loop(engine, token, tick_period, url));
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

#[instrument(skip(engine, token), fields(tick_period_ms = tick_period.as_millis() as u64, url = %url))]
async fn run_claim_fees_loop(
    engine: Engine,
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
                if let Err(err) = claim_fees(&engine, url.clone()).await {
                    error!(error = ?err, "Failed to claim fees");
                }
            },
            _ = token.cancelled() => break,
        }
    }
    info!("Claim fees task stopped");
}

#[instrument(skip(engine), fields(validator = %engine.authority()))]
pub async fn claim_fees(
    engine: &Engine,
    url: String,
) -> Result<(), ClaimFeesError> {
    info!("Claiming validator fees");

    let rpc_client =
        RpcClient::new_with_commitment(url, CommitmentConfig::confirmed());

    let validator = engine.authority();
    let signer = engine.signer();
    // Fee claiming only runs for standalone/primary engines, whose represented
    // authority is the local signer. Replicas may represent a remote authority
    // and must never claim its vault with their local identity.
    if validator != signer.pubkey() {
        return Err(ClaimFeesError::AuthoritySignerMismatch {
            authority: validator,
            signer: signer.pubkey(),
        });
    }
    let validator_fees_vault =
        dlp_api::pda::validator_fees_vault_pda_from_validator(&validator);
    let vault_lamports = rpc_client
        .get_balance(&validator_fees_vault)
        .await
        .map_err(|e| MagicBlockRpcClientError::RpcClientError(Box::new(e)))?;

    if vault_lamports <= MIN_CLAIMABLE_LAMPORTS {
        info!(
            validator_fees_vault = %validator_fees_vault,
            vault_lamports,
            min_claimable_lamports = MIN_CLAIMABLE_LAMPORTS,
            "Skipping validator fee claim below threshold"
        );
        return Ok(());
    }

    let ix = validator_claim_fees(validator, None);

    let latest_blockhash =
        rpc_client.get_latest_blockhash().await.map_err(|e| {
            MagicBlockRpcClientError::GetLatestBlockhash(Box::new(e))
        })?;

    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&validator),
        &[signer],
        latest_blockhash,
    );

    rpc_client
        .send_and_confirm_transaction(&tx)
        .await
        .map_err(|e| MagicBlockRpcClientError::SendTransaction(Box::new(e)))?;

    info!("Successfully claimed validator fees");

    Ok(())
}
