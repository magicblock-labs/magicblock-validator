use std::collections::HashMap;

use magicblock_magic_program_api as magic_program;
use magicblock_program::MagicContext;
use solana_account::{AccountBuilder, AccountMode, AccountSharedData};
use solana_native_token::LAMPORTS_PER_SOL;
use solana_program_option::COption;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use solana_rent::Rent;
use spl_token::{native_mint, state::Mint};

/// Builds validator-controlled accounts that must exist before execution starts.
pub(crate) fn initial_accounts(
    validator_id: Pubkey,
) -> HashMap<Pubkey, AccountSharedData> {
    let authority = AccountBuilder::default()
        .lamports(u64::MAX / 2)
        .mode(AccountMode::System)
        .build();
    let mut accounts = HashMap::from([(validator_id, authority)]);

    let magic_context = AccountBuilder::default()
        .lamports(u64::MAX)
        .data(vec![0; MagicContext::SIZE])
        .owner(magic_program::ID)
        .mode(AccountMode::Delegated)
        .build();
    accounts.insert(magic_program::MAGIC_CONTEXT_PUBKEY, magic_context);

    let vault = AccountBuilder::default()
        .lamports(Rent::default().minimum_balance(0))
        .owner(magic_program::ID)
        .mode(AccountMode::Ephemeral)
        .build();
    accounts.insert(magic_program::EPHEMERAL_VAULT_PUBKEY, vault);

    let mut native_mint_data = vec![0; Mint::LEN];
    Mint {
        mint_authority: COption::None,
        supply: 0,
        decimals: native_mint::DECIMALS,
        is_initialized: true,
        freeze_authority: COption::None,
    }
    .pack_into_slice(&mut native_mint_data);
    let native_mint = AccountBuilder::default()
        .lamports(LAMPORTS_PER_SOL)
        .data(native_mint_data)
        .owner(spl_token::id())
        .build();
    accounts.insert(native_mint::id(), native_mint);

    accounts
}
