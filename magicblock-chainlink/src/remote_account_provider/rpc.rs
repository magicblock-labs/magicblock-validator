use solana_account_decoder_client_types::UiAccountEncoding;
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::config::RpcAccountInfoConfig;
use std::time::Duration;

use crate::remote_account_provider::ChainRpcClient;
const DEFAULT_RPC_CALL_TIMEOUT: Duration = Duration::from_secs(2);

/// Fetches multiple accounts from the RPC with a configurable timeout.
/// Includes commitment level and min_context_slot parameters to ensure
/// up-to-date account state.
///
/// Returns the raw result from the RPC call with timeout handling.
/// Error handling (retries, slot validation) should be done by the caller.
pub async fn fetch_accounts_with_timeout<T>(
    rpc_client: &T,
    pubkeys: &[Pubkey],
    commitment: CommitmentConfig,
    min_context_slot: u64,
    timeout: Option<Duration>,
) -> Result<
    Result<
        solana_rpc_client_api::response::Response<
            Vec<Option<solana_account::Account>>,
        >,
        solana_rpc_client_api::client_error::Error,
    >,
    tokio::time::error::Elapsed,
>
where
    T: ChainRpcClient,
{
    tokio::time::timeout(
        timeout.unwrap_or(DEFAULT_RPC_CALL_TIMEOUT),
        rpc_client.get_multiple_accounts_with_config(
            pubkeys,
            RpcAccountInfoConfig {
                commitment: Some(commitment),
                min_context_slot: Some(min_context_slot),
                encoding: Some(UiAccountEncoding::Base64Zstd),
                data_slice: None,
            },
        ),
    )
    .await
}
