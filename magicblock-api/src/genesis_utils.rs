use solana_account::{Account, AccountSharedData};
use solana_keypair::Keypair;
use solana_native_token::LAMPORTS_PER_SOL;
use solana_program_option::COption;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use spl_token::{native_mint, state::Mint};

// Default amount received by the validator
const VALIDATOR_LAMPORTS: u64 = u64::MAX / 2;

pub struct GenesisConfigInfo {
    pub accounts: Vec<(Pubkey, AccountSharedData)>,
    pub validator_pubkey: Pubkey,
}

pub fn create_genesis_config_with_leader(
    mint_lamports: u64,
    validator_pubkey: &Pubkey,
) -> GenesisConfigInfo {
    let mint_keypair = Keypair::new();
    let token_program = spl_token::id();
    let native_mint = native_mint::id();
    let mut native_mint_data = [0; Mint::LEN];
    Mint {
        mint_authority: COption::None,
        supply: 0,
        decimals: native_mint::DECIMALS,
        is_initialized: true,
        freeze_authority: COption::None,
    }
    .pack_into_slice(&mut native_mint_data);
    let accounts = vec![
        (
            mint_keypair.pubkey(),
            AccountSharedData::new(mint_lamports, 0, &Pubkey::default()),
        ),
        (
            *validator_pubkey,
            AccountSharedData::new(VALIDATOR_LAMPORTS, 0, &Pubkey::default()),
        ),
        (
            native_mint,
            AccountSharedData::from(Account {
                owner: token_program,
                data: native_mint_data.to_vec(),
                lamports: LAMPORTS_PER_SOL,
                executable: false,
                rent_epoch: 1,
            }),
        ),
    ];

    GenesisConfigInfo {
        accounts,
        validator_pubkey: *validator_pubkey,
    }
}
