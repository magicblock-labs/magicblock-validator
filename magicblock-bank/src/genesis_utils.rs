// NOTE: from runtime/src/genesis_utils.rs
// heavily updated to remove vote + stake related code as well as cluster type (defaulting to mainnet)
use std::time::UNIX_EPOCH;

use solana_sdk::{
    account::{Account, AccountSharedData},
    clock::UnixTimestamp,
    feature::{self, Feature},
    feature_set::FeatureSet,
    fee_calculator::FeeRateGovernor,
    genesis_config::{ClusterType, GenesisConfig},
    native_token::sol_to_lamports,
    pubkey::Pubkey,
    rent::Rent,
    signature::{Keypair, Signer},
    stake::state::StakeStateV2,
    system_program,
};

use crate::DEFAULT_LAMPORTS_PER_SIGNATURE;

// Default amount received by the validator
const VALIDATOR_LAMPORTS: u64 = 42;

pub fn bootstrap_validator_stake_lamports() -> u64 {
    Rent::default().minimum_balance(StakeStateV2::size_of())
}

// Number of lamports automatically used for genesis accounts
pub const fn genesis_sysvar_and_builtin_program_lamports() -> u64 {
    const NUM_BUILTIN_PROGRAMS: u64 = 9;
    const NUM_PRECOMPILES: u64 = 2;
    const FEES_SYSVAR_MIN_BALANCE: u64 = 946_560;
    const CLOCK_SYSVAR_MIN_BALANCE: u64 = 1_169_280;
    const RENT_SYSVAR_MIN_BALANCE: u64 = 1_009_200;
    const EPOCH_SCHEDULE_SYSVAR_MIN_BALANCE: u64 = 1_120_560;
    const RECENT_BLOCKHASHES_SYSVAR_MIN_BALANCE: u64 = 42_706_560;

    FEES_SYSVAR_MIN_BALANCE
        + CLOCK_SYSVAR_MIN_BALANCE
        + RENT_SYSVAR_MIN_BALANCE
        + EPOCH_SCHEDULE_SYSVAR_MIN_BALANCE
        + RECENT_BLOCKHASHES_SYSVAR_MIN_BALANCE
        + NUM_BUILTIN_PROGRAMS
        + NUM_PRECOMPILES
}

pub struct GenesisConfigInfo {
    pub genesis_config: GenesisConfig,
    pub mint_keypair: Keypair,
    pub validator_pubkey: Pubkey,
}

pub fn create_genesis_config_with_leader(
    mint_lamports: u64,
    validator_pubkey: &Pubkey,
    lamports_per_signature: Option<u64>,
) -> GenesisConfigInfo {
    let mint_keypair = Keypair::new();

    let genesis_config = create_genesis_config_with_leader_ex(
        mint_lamports,
        &mint_keypair.pubkey(),
        validator_pubkey,
        VALIDATOR_LAMPORTS,
        FeeRateGovernor {
            target_lamports_per_signature: 0,
            lamports_per_signature: lamports_per_signature
                .unwrap_or(DEFAULT_LAMPORTS_PER_SIGNATURE),
            target_signatures_per_slot: 0,
            ..FeeRateGovernor::default()
        },
        Rent::free(),
        vec![],
    );

    GenesisConfigInfo {
        genesis_config,
        mint_keypair,
        validator_pubkey: *validator_pubkey,
    }
}

pub fn activate_all_features(genesis_config: &mut GenesisConfig) {
    // Activate all features at genesis in development mode
    for feature_id in FeatureSet::default().inactive {
        activate_feature(genesis_config, feature_id);
    }
}

pub fn activate_feature(
    genesis_config: &mut GenesisConfig,
    feature_id: Pubkey,
) {
    genesis_config.accounts.insert(
        feature_id,
        Account::from(feature::create_account(
            &Feature {
                activated_at: Some(0),
            },
            std::cmp::max(
                genesis_config.rent.minimum_balance(Feature::size_of()),
                1,
            ),
        )),
    );
}

#[allow(clippy::too_many_arguments)]
pub fn create_genesis_config_with_leader_ex(
    mint_lamports: u64,
    mint_pubkey: &Pubkey,
    validator_pubkey: &Pubkey,
    validator_lamports: u64,
    fee_rate_governor: FeeRateGovernor,
    rent: Rent,
    mut initial_accounts: Vec<(Pubkey, AccountSharedData)>,
) -> GenesisConfig {
    initial_accounts.push((
        *mint_pubkey,
        AccountSharedData::new(mint_lamports, 0, &system_program::id()),
    ));
    initial_accounts.push((
        *validator_pubkey,
        AccountSharedData::new(validator_lamports, 0, &system_program::id()),
    ));

    // Note that zero lamports for validator stake will result in stake account
    // not being stored in accounts-db but still cached in bank stakes. This
    // causes discrepancy between cached stakes accounts in bank and
    // accounts-db which in particular will break snapshots test.
    let native_mint_account =
        solana_sdk::account::AccountSharedData::from(Account {
            owner: solana_inline_spl::token::id(),
            data: solana_inline_spl::token::native_mint::ACCOUNT_DATA.to_vec(),
            lamports: sol_to_lamports(1.),
            executable: false,
            rent_epoch: 1,
        });
    initial_accounts.push((
        solana_inline_spl::token::native_mint::id(),
        native_mint_account,
    ));

    let mut genesis_config = GenesisConfig {
        accounts: initial_accounts
            .iter()
            .cloned()
            .map(|(key, account)| (key, Account::from(account)))
            .collect(),
        fee_rate_governor,
        rent,
        cluster_type: ClusterType::MainnetBeta,
        creation_time: UNIX_EPOCH.elapsed().unwrap().as_secs() as UnixTimestamp,
        ..GenesisConfig::default()
    };

    if genesis_config.cluster_type == ClusterType::Development {
        activate_all_features(&mut genesis_config);
    }

    genesis_config
}
