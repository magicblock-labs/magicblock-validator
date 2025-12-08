pub use magicblock_core::token_programs::{
    derive_ata, derive_eata, EATA_PROGRAM_ID, SPL_TOKEN_PROGRAM_ID,
};
use solana_account::Account;
use solana_program::{program_option::COption, program_pack::Pack};
use solana_pubkey::Pubkey;
use solana_sdk::rent::Rent;
use spl_token::state::{Account as SplAccount, AccountState};

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
        owner: SPL_TOKEN_PROGRAM_ID,
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

    let owner = if delegate { dlp::ID } else { EATA_PROGRAM_ID };

    Account {
        owner,
        data,
        lamports,
        ..Default::default()
    }
}

/// Internal representation of a token account data.
#[repr(C)]
pub struct EphemeralAta {
    /// The owner of the eata
    pub owner: Pubkey,
    /// The mint associated with this account
    pub mint: Pubkey,
    /// The amount of tokens this account holds.
    pub amount: u64,
}
