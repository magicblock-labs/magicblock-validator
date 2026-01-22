mod counter_sdk;
mod delegation_sdk;
mod metrics;
mod test_utils;

use std::time::Duration;

use anyhow::Result;
use solana_sdk::signer::Signer;
use tracing::info;

use counter_sdk::{
    build_delegate_ix, build_increment_and_undelegate_ix, build_init_ix,
    derive_counter_pda, read_counter_value, COUNTER_PROGRAM_ID,
};
use delegation_sdk::{
    build_direct_undelegate_flow, delegation_metadata_pda,
    fetch_delegation_metadata, fetch_delegation_record, is_delegated,
};
use metrics::fetch_metrics;
use test_utils::{create_rpc_clients, load_keypairs, send_and_confirm};

const METRICS_ENDPOINT: &str = "http://127.0.0.1:9000/metrics";
const COUNTER_ID: u64 = 0;

#[tokio::main]
async fn main() -> Result<()> {
    magicblock_core::logger::init();

    // ===== SETUP =====
    info!("Phase 0: Setting up connections and keypairs");

    let (payer, validator) = load_keypairs()?;
    let (devnet_rpc, ephemeral_rpc) = create_rpc_clients()?;

    let payer_pubkey = payer.pubkey();
    let validator_pubkey = validator.pubkey();

    info!("Payer: {}", payer_pubkey);
    info!("Validator: {}", validator_pubkey);

    let (counter_pda, _) = derive_counter_pda(COUNTER_ID);
    info!("Counter PDA: {}", counter_pda);

    // ===== PHASE 1: Initialize Counter (if needed) =====
    info!("Phase 1: Ensuring counter is initialized on devnet");

    let counter_exists = devnet_rpc.get_account(&counter_pda).is_ok();
    if !counter_exists {
        info!("Counter does not exist, initializing...");
        let init_ix = build_init_ix(&payer_pubkey, COUNTER_ID);
        send_and_confirm(&devnet_rpc, &[init_ix], &payer, &[])?;
        info!("Counter initialized on devnet");
    } else {
        info!("Counter already exists on devnet");
    }

    let initial_counter = read_counter_value(&devnet_rpc, COUNTER_ID)?;
    info!("Initial counter - count: {}", initial_counter.count);

    // ===== PHASE 2: Fetch Initial Metrics =====
    info!("Phase 2: Recording initial metrics");

    let initial_metrics = fetch_metrics(METRICS_ENDPOINT).await?;
    info!(
        "Initial pubkey updates: {}, account updates: {}",
        initial_metrics.pubkey_updates_count,
        initial_metrics.account_updates_count
    );

    // ===== PHASE 3: Delegate Counter to Validator =====
    info!("Phase 3: Delegating counter to validator");

    if is_delegated(&devnet_rpc, &counter_pda) {
        info!("Counter is already delegated, skipping delegation");
    } else {
        let delegate_ix = build_delegate_ix(
            &payer_pubkey,
            COUNTER_ID,
            Some(&validator_pubkey),
        );
        send_and_confirm(&devnet_rpc, &[delegate_ix], &payer, &[])?;
        info!("Counter delegated to validator");
    }

    tokio::time::sleep(Duration::from_secs(2)).await;

    // ===== PHASE 4: Clone & Increment on Ephemeral (Normal Flow) =====
    info!("Phase 4: Cloning and incrementing counter on ephemeral validator");

    let _ = ephemeral_rpc.get_account(&counter_pda);
    info!("Triggered clone by requesting account");

    tokio::time::sleep(Duration::from_secs(3)).await;

    let cloned_counter = read_counter_value(&ephemeral_rpc, COUNTER_ID)?;
    info!(
        "Counter on ephemeral after clone - count: {}",
        cloned_counter.count
    );

    // ===== PHASE 5: Undelegate via Ephemeral (Normal Flow) =====
    info!(
        "Phase 5: Undelegating counter via ephemeral (increment + undelegate)"
    );

    let undelegate_ix =
        build_increment_and_undelegate_ix(&payer_pubkey, COUNTER_ID);
    send_and_confirm(&ephemeral_rpc, &[undelegate_ix], &payer, &[])?;
    info!("Increment+undelegate scheduled on ephemeral");

    info!("Waiting for undelegation to finalize on devnet...");
    tokio::time::sleep(Duration::from_secs(15)).await;

    // ===== PHASE 6: Verify Counter Updated on Devnet =====
    info!("Phase 6: Verifying counter updated on devnet");

    let final_counter = read_counter_value(&devnet_rpc, COUNTER_ID)?;
    info!("Final counter on devnet - count: {}", final_counter.count);

    assert!(
        final_counter.count > initial_counter.count,
        "Counter should be incremented after undelegation"
    );
    info!("✓ Counter value correctly updated on devnet");

    // ===== PHASE 7: Delegate Again for Cheating Flow =====
    info!("Phase 7: Delegating counter again for cheating flow test");

    let delegate_ix2 =
        build_delegate_ix(&payer_pubkey, COUNTER_ID, Some(&validator_pubkey));
    send_and_confirm(&devnet_rpc, &[delegate_ix2], &payer, &[])?;
    info!("Counter delegated again");

    tokio::time::sleep(Duration::from_secs(2)).await;

    // ===== PHASE 8: Direct Undelegate on Devnet (Cheating Flow) =====
    info!("Phase 8: Direct undelegation on devnet (cheating flow)");
    info!("Note: Account was delegated but NEVER cloned to ephemeral");

    let delegation_metadata =
        fetch_delegation_metadata(&devnet_rpc, &counter_pda)?;
    let delegation_record = fetch_delegation_record(&devnet_rpc, &counter_pda)?;

    info!(
        "Delegation metadata - nonce: {}, is_undelegatable: {}, rent_payer: {}",
        delegation_metadata.last_update_nonce,
        delegation_metadata.is_undelegatable,
        delegation_metadata.rent_payer
    );
    info!(
        "Delegation record - owner: {}, authority: {}",
        delegation_record.owner, delegation_record.authority
    );

    let delegated_account_data = devnet_rpc.get_account(&counter_pda)?;
    info!(
        "Delegated account - lamports: {}, data len: {}",
        delegated_account_data.lamports,
        delegated_account_data.data.len()
    );

    let cheating_ixs = build_direct_undelegate_flow(
        validator_pubkey,
        counter_pda,
        COUNTER_PROGRAM_ID,
        delegation_metadata.rent_payer,
        delegated_account_data.data.clone(),
        delegated_account_data.lamports,
        delegation_metadata.last_update_nonce + 1,
    );

    send_and_confirm(&devnet_rpc, &cheating_ixs, &payer, &[&validator])?;
    info!("Direct undelegation completed on devnet (cheating flow)");

    // ===== PHASE 9: Verify Metrics =====
    info!("Phase 9: Verifying metrics updates");

    tokio::time::sleep(Duration::from_secs(5)).await;

    let final_metrics = fetch_metrics(METRICS_ENDPOINT).await?;
    let pubkey_updates_delta = final_metrics
        .pubkey_updates_count
        .saturating_sub(initial_metrics.pubkey_updates_count);
    let account_updates_delta = final_metrics
        .account_updates_count
        .saturating_sub(initial_metrics.account_updates_count);

    info!(
        "Final pubkey updates: {} (delta: {})",
        final_metrics.pubkey_updates_count, pubkey_updates_delta
    );
    info!(
        "Final account updates: {} (delta: {})",
        final_metrics.account_updates_count, account_updates_delta
    );

    assert!(
        pubkey_updates_delta >= 2,
        "Expected at least 2 pubkey updates (both undelegations), got {}",
        pubkey_updates_delta
    );
    info!("✓ Pubkey updates tracking verified");

    assert!(
        account_updates_delta >= 1,
        "Expected at least 1 account update (cloned account), got {}",
        account_updates_delta
    );
    info!("✓ Account updates tracking verified");

    // ===== PHASE 10: Verify Final State =====
    info!("Phase 10: Verifying final counter state");

    let final_counter_state = read_counter_value(&devnet_rpc, COUNTER_ID)?;
    info!("Final counter state - count: {}", final_counter_state.count);

    let delegation_metadata_pda = delegation_metadata_pda(&counter_pda);
    let metadata_exists =
        devnet_rpc.get_account(&delegation_metadata_pda).is_ok();

    assert!(
        !metadata_exists,
        "Delegation metadata should be closed after undelegation"
    );
    info!("✓ Delegation metadata correctly closed");

    info!("✓ All assertions passed!");
    info!("✓ Test completed successfully!");

    Ok(())
}
