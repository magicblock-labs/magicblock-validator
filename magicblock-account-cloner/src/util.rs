use std::sync::Arc;

use magicblock_committor_service::BaseIntentCommittor;
use magicblock_rpc_client::MagicblockRpcClient;
use solana_signature::Signature;

pub(crate) async fn get_tx_diagnostics<C: BaseIntentCommittor>(
    sig: &Signature,
    committor: &Arc<C>,
) -> (Option<Vec<String>>, Option<u64>) {
    if let Ok(Ok(transaction)) = committor.get_transaction(sig).await {
        let cus = MagicblockRpcClient::get_cus_from_transaction(&transaction);
        let logs = MagicblockRpcClient::get_logs_from_transaction(&transaction);
        (logs, cus)
    } else {
        (None, None)
    }
}
