use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::config::RpcTransactionConfig;
use solana_sdk::{commitment_config::CommitmentConfig, signature::Signature};

pub async fn tx_logs_contain(
    rpc_client: &RpcClient,
    signature: &Signature,
    needle: &str,
) -> bool {
    // NOTE: we encountered the following error a few times which makes tests fail for the
    //       wrong reason:
    //       Error {
    //          request: Some(GetTransaction),
    //          kind: SerdeJson( Error(
    //              "invalid type: null,
    //              expected struct EncodedConfirmedTransactionWithStatusMeta",
    //              line: 0, column: 0))
    //      }
    //      Therefore we retry a few times.
    const MAX_RETRIES: usize = 5;
    let mut retries = MAX_RETRIES;
    let tx = loop {
        match rpc_client
            .get_transaction_with_config(
                signature,
                RpcTransactionConfig {
                    commitment: Some(CommitmentConfig::confirmed()),
                    max_supported_transaction_version: Some(0),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(tx) => break tx,
            Err(err) => {
                log::error!("Failed to get transaction: {}", err);
                retries -= 1;
                if retries == 0 {
                    panic!(
                        "Failed to get transaction after {} retries",
                        MAX_RETRIES
                    );
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(100))
                    .await;
            }
        };
    };
    let logs = tx
        .transaction
        .meta
        .as_ref()
        .unwrap()
        .log_messages
        .clone()
        .unwrap_or_else(Vec::new);
    logs.iter().any(|log| log.contains(needle))
}
