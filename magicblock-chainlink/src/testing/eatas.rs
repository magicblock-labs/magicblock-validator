pub use magicblock_core::token_programs::{
    derive_ata, derive_eata, EphemeralAta, EATA_PROGRAM_ID, TOKEN_PROGRAM_ID,
};
use solana_account::Account;
use solana_program::{program_option::COption, program_pack::Pack};
use solana_pubkey::Pubkey;
use solana_sysvar::rent::Rent;
use spl_token::state::{Account as SplAccount, AccountState};

/// Creates a test ATA (Associated Token Account) with initialized state and zero balance.
///
/// # Arguments
/// * `owner` - The public key of the account owner
/// * `mint` - The public key of the token mint
pub fn create_ata_account(owner: &Pubkey, mint: &Pubkey) -> Account {
    let token_account = SplAccount {
        mint: *mint,
        owner: *owner,
        amount: 0,
        delegate: COption::None,
        state: AccountState::Initialized,
        is_native: COption::None,
        delegated_amount: 0,
        close_authority: COption::None,
    };

    let mut data = vec![0u8; SplAccount::LEN];
    SplAccount::pack(token_account, &mut data).expect("pack spl token account");
    let lamports = Rent::default().minimum_balance(data.len());

    Account {
        owner: TOKEN_PROGRAM_ID,
        data,
        lamports,
        executable: false,
        ..Default::default()
    }
}

pub fn create_eata_account(
    owner: &Pubkey,
    mint: &Pubkey,
    amount: u64,
    delegate: bool,
) -> Account {
    let mut data = Vec::with_capacity(64 + 8);
    data.extend_from_slice(owner.as_ref());
    data.extend_from_slice(mint.as_ref());
    data.extend_from_slice(&amount.to_le_bytes());
    let lamports = Rent::default().minimum_balance(data.len());

    let account_owner = if delegate { dlp::id() } else { EATA_PROGRAM_ID };

    Account {
        owner: account_owner,
        data,
        lamports,
        ..Default::default()
    }
}

// Reuse EphemeralAta definition from magicblock_core::token_programs
