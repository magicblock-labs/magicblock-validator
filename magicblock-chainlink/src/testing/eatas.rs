use solana_account::Account;
use solana_program::{program_option::COption, program_pack::Pack};
use solana_pubkey::{pubkey, Pubkey};
use solana_sdk::rent::Rent;
use spl_token::state::Account as SplAccount;
use spl_token::state::AccountState;

const SPL_TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

// Associated Token Program id
const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

// eATA PDA derivation seed copied from fetch_cloner.rs
const EATA_PROGRAM_ID: Pubkey =
    pubkey!("5iC4wKZizyxrKh271Xzx3W4Vn2xUyYvSGHeoB2mdw5HA");

pub fn derive_ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    let (addr, _bump) = Pubkey::find_program_address(
        &[owner.as_ref(), SPL_TOKEN_PROGRAM_ID.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    );
    addr
}

pub fn derive_eata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    let (addr, _bump) = Pubkey::find_program_address(
        &[owner.as_ref(), mint.as_ref()],
        &EATA_PROGRAM_ID,
    );
    addr
}

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
