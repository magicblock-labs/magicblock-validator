use solana_rpc_client_api::client_error::Result as ClientResult;

use solana_rpc_client::rpc_client::RpcClient;
use solana_sdk::{commitment_config::CommitmentConfig, signature::Signature};

use crate::TriggerCommitTestContext;

pub fn commit_to_chain_failed_with_invalid_account_owner(
    res: ClientResult<Signature>,
    commitment: CommitmentConfig,
) {
    let ctx = TriggerCommitTestContext::new();
    let (chain_sig, ephem_logs) = match res {
        Ok(sig) => {
            let logs =
                ctx.fetch_logs(sig, None).expect("Failed to extract logs");
            let chain_sig = ctx.extract_chain_transaction_signature(&logs);
            (chain_sig, logs)
        }
        Err(err) => {
            panic!("{:?}", err);
        }
    };
    eprintln!("Ephemeral logs: ");
    eprintln!("{:#?}", ephem_logs);

    let chain_sig = chain_sig.unwrap_or_else(|| {
        panic!(
            "Chain transaction signature not found in logs, {:#?}",
            ephem_logs
        )
    });

    let devnet_client = RpcClient::new_with_commitment(
        "https://api.devnet.solana.com".to_string(),
        commitment,
    );

    // Wait for tx on devnet to confirm and then get its logs
    let chain_logs = match ctx
        .confirm_transaction(&chain_sig, Some(&devnet_client))
    {
        Ok(res) => {
            eprintln!("Chain transaction confirmed with success: '{:?}'", res);
            ctx.fetch_logs(chain_sig, Some(&devnet_client))
        }
        Err(err) => panic!("Chain transaction failed to confirm: {:?}", err),
    };

    eprintln!("Chain logs: ");
    eprintln!("{:#?}", chain_logs);

    assert!(chain_logs.is_some());
    assert!(chain_logs
        .unwrap()
        .into_iter()
        .any(|log| { log.contains("failed: Invalid account owner") }));
}
