use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    account::Account, bpf_loader_upgradeable,
    commitment_config::CommitmentConfig, pubkey::Pubkey,
};

use crate::Cluster;

const ANCHOR_SEED: &str = "anchor:idl";
const SHANK_SEED: &str = "shank:idl";

pub async fn fetch_account(
    cluster: &Cluster,
    pubkey: &Pubkey,
) -> Result<Account, solana_rpc_client_api::client_error::Error> {
    // TODO(vbrunet)
    //  - Long term this should probably use the validator's AccountFetcher
    //  - Tracked here: https://github.com/magicblock-labs/magicblock-validator/issues/136
    let rpc_client = RpcClient::new_with_commitment(
        cluster.url().to_string(),
        CommitmentConfig::confirmed(),
    );
    rpc_client.get_account(pubkey).await
}

pub fn get_pubkey_anchor_idl(program_id: &Pubkey) -> Option<Pubkey> {
    let (base, _) = Pubkey::find_program_address(&[], program_id);
    Pubkey::create_with_seed(&base, ANCHOR_SEED, program_id).ok()
}

pub fn get_pubkey_shank_idl(program_id: &Pubkey) -> Option<Pubkey> {
    let (base, _) = Pubkey::find_program_address(&[], program_id);
    Pubkey::create_with_seed(&base, SHANK_SEED, program_id).ok()
}

pub fn get_pubkey_program_data(program_id: &Pubkey) -> Pubkey {
    let bpf_loader_id = bpf_loader_upgradeable::id();
    let seeds: &[_; 1] = &[program_id.as_ref()];
    let (executable_address, _) =
        Pubkey::find_program_address(seeds, &bpf_loader_id);
    executable_address
}
