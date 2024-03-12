pub use sleipnir_bank::genesis_utils::{
    create_genesis_config_with_leader, GenesisConfigInfo,
};

// same as genesis_config::create_genesis_config, but with bootstrap_validator staking logic
//  for the core crate tests
pub fn create_genesis_config(mint_lamports: u64) -> GenesisConfigInfo {
    create_genesis_config_with_leader(
        mint_lamports,
        &solana_sdk::pubkey::new_rand(),
    )
}
