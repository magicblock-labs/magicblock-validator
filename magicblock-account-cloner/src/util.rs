use std::sync::Arc;

use magicblock_committor_service::BaseIntentCommittor;
use magicblock_program::validator::validator_authority_id;
use magicblock_rpc_client::MagicblockRpcClient;
use solana_pubkey::Pubkey;
use solana_signature::Signature;

/// Seed for deriving buffer account PDA
const BUFFER_SEED: &[u8] = b"buffer";

/// Derives a deterministic buffer account pubkey for program cloning.
/// Uses validator_authority as owner so it works for any loader type.
pub fn derive_buffer_pubkey(program_pubkey: &Pubkey) -> (Pubkey, u8) {
    let seeds: &[&[u8]] = &[BUFFER_SEED, program_pubkey.as_ref()];
    Pubkey::find_program_address(seeds, &validator_authority_id())
}

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
