//! One-shot base-chain fee claims using the leader's configured identity.

use anyhow::{Context, Result};
use config::LeaderParams;
use delegation_program_interface::{
    self as dpi, legacy::instruction_builder::validator_claim_fees,
};
use solana_commitment_config::CommitmentConfig;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;
use solana_transaction::Transaction;
use tracing::info;

/// Vault-balance floor for submitting a fee-paying claim transaction.
const MIN_CLAIMABLE_LAMPORTS: u64 = 100_000_000;

/// Claims and confirms once, with the leader identity as authority, payer, and recipient.
/// A vault balance at or below the floor is a successful no-op, not a retry condition.
pub(super) async fn run(config: LeaderParams) -> Result<()> {
    let rpc =
        RpcClient::new_with_commitment(config.rpc_url().to_owned(), CommitmentConfig::confirmed());
    // Leader config validation rejects remote authorities, so the local
    // signer is also the authority whose vault we can claim.
    let signer = config.engine.authority.local;
    let validator = signer.pubkey();
    let vault = dpi::legacy::pda::validator_fees_vault_pda_from_validator(&validator);
    let balance = rpc
        .get_balance(&vault)
        .await
        .with_context(|| format!("failed to read fee vault {vault}"))?;
    if balance <= MIN_CLAIMABLE_LAMPORTS {
        info!(balance, "Fee claim skipped: balance at or below threshold");
        return Ok(());
    }

    let blockhash = rpc
        .get_latest_blockhash()
        .await
        .context("failed to get latest blockhash for fee claim")?;
    let mut transaction =
        Transaction::new_with_payer(&[validator_claim_fees(validator, None)], Some(&validator));
    transaction
        .try_sign(&[signer.as_ref()], blockhash)
        .context("failed to sign validator fee claim")?;
    let signature = rpc
        .send_and_confirm_transaction(&transaction)
        .await
        .with_context(|| format!("failed to send and confirm fee claim for {validator}"))?;
    info!(%signature, "Fee claim confirmed");
    Ok(())
}
