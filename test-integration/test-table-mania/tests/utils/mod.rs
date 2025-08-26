use std::time::{Duration, Instant};

use log::*;
use magicblock_rpc_client::MagicblockRpcClient;
use magicblock_table_mania::{GarbageCollectorConfig, TableMania};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig, native_token::LAMPORTS_PER_SOL,
    signature::Keypair, signer::Signer,
};

#[allow(unused)] // used in tests
pub const TEST_TABLE_CLOSE: bool = cfg!(feature = "test_table_close");

pub async fn sleep_millis(millis: u64) {
    tokio::time::sleep(tokio::time::Duration::from_millis(millis)).await;
}

#[allow(unused)] // used in tests
pub async fn setup_table_mania(validator_auth: &Keypair) -> TableMania {
    let rpc_client = {
        let client = RpcClient::new_with_commitment(
            "http://localhost:7799".to_string(),
            CommitmentConfig::processed(),
        );
        MagicblockRpcClient::from(client)
    };
    rpc_client
        .request_airdrop(&validator_auth.pubkey(), 777 * LAMPORTS_PER_SOL)
        .await
        .unwrap();

    if TEST_TABLE_CLOSE {
        TableMania::new(
            rpc_client,
            validator_auth,
            Some(GarbageCollectorConfig::default()),
        )
    } else {
        TableMania::new(rpc_client, validator_auth, None)
    }
}

#[allow(unused)] // used in tests
pub async fn close_released_tables(table_mania: &TableMania) {
    if TEST_TABLE_CLOSE {
        // Tables deactivate after ~2.5 mins (150secs), but most times
        // it takes a lot longer so we allow double the time
        const MAX_TIME_TO_CLOSE: Duration = Duration::from_secs(300);

        info!(
            "Waiting for table close for up to {} secs",
            MAX_TIME_TO_CLOSE.as_secs()
        );
        let start = Instant::now();
        let mut count = 0;
        let releasing_pubkeys = table_mania.released_table_addresses().await;

        while table_mania.released_tables_count().await > 0 {
            if Instant::now() - start > MAX_TIME_TO_CLOSE {
                panic!("Timed out waiting for table close");
            }
            count += 1;
            if count % 10 == 0 {
                debug!(
                    "Still waiting to close, {} released tables",
                    table_mania.released_tables_count().await
                );
            }
            sleep_millis(10_000).await;
        }

        for released_pubkey in releasing_pubkeys {
            let table = table_mania
                .rpc_client
                .get_account(&released_pubkey)
                .await
                .expect("Failed to get table account");
            assert!(
                table.is_none(),
                "Table {} not closed on chain",
                released_pubkey
            );
        }
    } else {
        info!("Skipping table close wait");
    }
}

#[allow(unused)] // used in tests
pub async fn log_active_table_addresses(table_mania: &TableMania) {
    debug!(
        "Active Tables: {}",
        table_mania
            .active_table_addresses()
            .await
            .into_iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
}
