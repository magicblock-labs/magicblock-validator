use std::time::Duration;

use dlp_api::instruction_builder::validator_claim_fees;
use engine::Engine;
use magicblock_rpc_client::MagicBlockRpcClientError;
use nucleus::shutdown::{ShutdownHandle, ShutdownReason};
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;
use solana_transaction::Transaction;
use tokio::time::Instant;
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

#[instrument(skip(engine, shutdown), fields(tick_period_ms = tick_period.as_millis() as u64, url = %url))]
pub async fn run_claim_fees_loop(
    engine: Engine,
    mut shutdown: ShutdownHandle,
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
            _ = shutdown.signalled() => break,
        }
    }
    info!("Claim fees task stopped");
    shutdown.terminate(ShutdownReason::Signalled);
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
