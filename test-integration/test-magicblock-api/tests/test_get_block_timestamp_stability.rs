use std::time::Duration;

use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    native_token::LAMPORTS_PER_SOL, signature::Keypair, signer::Signer,
};
use solana_transaction_status::UiTransactionEncoding;

const EPHEM_URL: &str = "http://localhost:8899";

#[tokio::test]
async fn test_get_block_timestamp_stability() {
    let rpc_client = RpcClient::new(EPHEM_URL.to_string());

    // Send a transaction to the validator
    let pubkey = Keypair::new().pubkey();
    let signature = rpc_client
        .request_airdrop(&pubkey, LAMPORTS_PER_SOL)
        .await
        .unwrap();
    let tx = rpc_client
        .get_transaction(&signature, UiTransactionEncoding::Base64)
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_secs(1)).await;

    let current_slot = tx.slot;
    let block_time = rpc_client.get_block_time(current_slot).await.unwrap();
    let ledger_block = rpc_client.get_block(current_slot).await.unwrap();

    assert_eq!(ledger_block.block_time, Some(block_time));
}
