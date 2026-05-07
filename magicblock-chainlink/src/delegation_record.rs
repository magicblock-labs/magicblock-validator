use dlp_api::{
    pda::delegation_record_pda_from_delegated_account, state::DelegationRecord,
};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::config::RpcAccountInfoConfig;

use crate::remote_account_provider::ChainRpcClient;

pub(crate) fn is_delegated_to_validator_or_confined(
    authority: &Pubkey,
    validator_pubkey: &Pubkey,
) -> bool {
    authority == validator_pubkey || authority == &Pubkey::default()
}

#[allow(dead_code)]
pub(crate) fn parse_delegation_record_header(
    data: &[u8],
) -> Option<DelegationRecord> {
    let delegation_record_size = DelegationRecord::size_with_discriminator();
    if data.len() < delegation_record_size {
        return None;
    }

    DelegationRecord::try_from_bytes_with_discriminator(
        &data[..delegation_record_size],
    )
    .copied()
    .ok()
}

#[allow(dead_code)]
pub(crate) async fn fetch_delegation_record_header<T: ChainRpcClient>(
    rpc_client: &T,
    delegated_account_pubkey: Pubkey,
    min_context_slot: u64,
) -> Option<DelegationRecord> {
    let delegation_record_pubkey =
        delegation_record_pda_from_delegated_account(&delegated_account_pubkey);
    let account = rpc_client
        .get_account_with_config(
            &delegation_record_pubkey,
            RpcAccountInfoConfig {
                commitment: Some(rpc_client.commitment()),
                min_context_slot: Some(min_context_slot),
                ..Default::default()
            },
        )
        .await
        .ok()?
        .value?;

    parse_delegation_record_header(&account.data)
}
