use dlp_api::state::DelegationRecord;
use magicblock_program::validator::validator_authority;
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;
use tracing::*;

use crate::errors::{ApiError, ApiResult};

pub(crate) fn delegation_record_authority(
    data: &[u8],
    delegation_record_pubkey: Pubkey,
) -> Result<Pubkey, String> {
    let delegation_record_size = DelegationRecord::size_with_discriminator();
    if data.len() < delegation_record_size {
        return Err(format!(
            "delegation record {delegation_record_pubkey} is too small"
        ));
    }

    DelegationRecord::try_from_bytes_with_discriminator(
        &data[..delegation_record_size],
    )
    .copied()
    .map(|record| record.authority)
    .map_err(|err| {
        format!(
            "failed to decode delegation record {delegation_record_pubkey}: {err:?}"
        )
    })
}

/// Delegates the task scheduler faucet to this validator on the base chain if
/// it is not already delegated. Delegating gives the faucet real (base-chain
/// backed) lamports usable inside the ephemeral rollup, unlike the validator
/// identity. The faucet must already be funded — the validator does not fund
/// it (operators and integration tests airdrop to it).
pub(crate) async fn ensure_faucet_delegated_on_chain(
    rpc_url: String,
    faucet: &Keypair,
) -> ApiResult<()> {
    let validator_keypair = validator_authority();
    let validator_pubkey = validator_keypair.pubkey();
    let faucet_pubkey = faucet.pubkey();
    let delegation_record_pubkey =
        dlp_api::pda::delegation_record_pda_from_delegated_account(
            &faucet_pubkey,
        );

    let rpc =
        RpcClient::new_with_commitment(rpc_url, CommitmentConfig::confirmed());

    let accounts = rpc
        .get_multiple_accounts(&[faucet_pubkey, delegation_record_pubkey])
        .await
        .map_err(|err| {
            ApiError::FailedToDelegateFaucet(faucet_pubkey, err.to_string())
        })?;

    // The faucet must be funded out-of-band before it can be delegated
    // (operators and integration tests airdrop to it; the validator never funds
    // it). If the account does not exist on the base chain it was never set up,
    // so there is nothing to delegate — skip instead of failing startup.
    // Validators that do not use hydra cranks rely on this: the default faucet
    // keypair is always present in the config but is left unfunded.
    let Some(faucet_account) = accounts[0].as_ref() else {
        warn!(
            %faucet_pubkey,
            "Crank faucet account not found on the base chain; skipping delegation. \
             Fund the faucet to enable hydra cranks."
        );
        return Ok(());
    };

    if faucet_account.owner == dlp_api::id() {
        let Some(delegation_record) = accounts[1].as_ref() else {
            return Err(ApiError::FailedToDelegateFaucet(
                faucet_pubkey,
                format!(
                    "faucet is owned by the delegation program but missing delegation record {delegation_record_pubkey}"
                ),
            ));
        };
        let authority = delegation_record_authority(
            &delegation_record.data,
            delegation_record_pubkey,
        )
        .map_err(|err| ApiError::FailedToDelegateFaucet(faucet_pubkey, err))?;
        if authority == validator_pubkey {
            info!(%faucet_pubkey, %validator_pubkey, "Crank faucet already delegated, skipping");
            return Ok(());
        }
        return Err(ApiError::FailedToDelegateFaucet(
            faucet_pubkey,
            format!(
                "faucet already delegated to validator {authority}, expected {validator_pubkey}"
            ),
        ));
    }

    info!(%faucet_pubkey, "Delegating crank faucet");
    // Hand the on-curve faucet to the delegation program and delegate it to
    // this validator. This makes it a writable, base-chain-backed account
    // inside the ephemeral rollup that can sponsor crank creation (mirrors the
    // on-curve delegation flow used in tests). The faucet must already be
    // funded; the validator does not fund it.
    let assign_ix = solana_system_interface::instruction::assign(
        &faucet_pubkey,
        &dlp_api::id(),
    );
    let delegate_ix = dlp_api::instruction_builder::delegate(
        validator_pubkey,
        faucet_pubkey,
        None,
        dlp_api::args::DelegateArgs {
            commit_frequency_ms: u32::MAX,
            seeds: vec![],
            validator: Some(validator_pubkey),
        },
    );

    let blockhash = rpc.get_latest_blockhash().await.map_err(|err| {
        ApiError::FailedToDelegateFaucet(faucet_pubkey, err.to_string())
    })?;
    let tx = solana_transaction::Transaction::new_signed_with_payer(
        &[assign_ix, delegate_ix],
        Some(&validator_pubkey),
        &[&validator_keypair, faucet],
        blockhash,
    );
    rpc.send_and_confirm_transaction(&tx).await.map_err(|err| {
        ApiError::FailedToDelegateFaucet(faucet_pubkey, err.to_string())
    })?;
    info!(%faucet_pubkey, "Crank faucet delegated");
    Ok(())
}
