pub use magicblock_core::token_programs::{
    derive_ata, derive_ata_with_token_program, derive_eata, EphemeralAta,
    EATA_PROGRAM_ID, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID,
};
use solana_account::Account;
use solana_program::{program_option::COption, program_pack::Pack};
use solana_pubkey::Pubkey;
use solana_rent::Rent;
use spl_token::state::{Account as SplAccount, AccountState};

/// Creates a test ATA (Associated Token Account) with initialized state and zero balance.
///
/// # Arguments
/// * `owner` - The public key of the account owner
/// * `mint` - The public key of the token mint
pub fn create_ata_account(owner: &Pubkey, mint: &Pubkey) -> Account {
    create_ata_account_with_token_program(
        owner,
        mint,
        TOKEN_PROGRAM_ID,
        SplAccount::LEN,
    )
}

pub fn create_token_2022_ata_account(owner: &Pubkey, mint: &Pubkey) -> Account {
    create_ata_account_with_token_program(
        owner,
        mint,
        TOKEN_2022_PROGRAM_ID,
        187,
    )
}

fn create_ata_account_with_token_program(
    owner: &Pubkey,
    mint: &Pubkey,
    token_program: Pubkey,
    data_len: usize,
) -> Account {
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

    let mut packed = vec![0u8; SplAccount::LEN];
    SplAccount::pack(token_account, &mut packed)
        .expect("pack spl token account");
    let mut data = vec![0u8; data_len.max(SplAccount::LEN)];
    data[..SplAccount::LEN].copy_from_slice(&packed);
    let lamports = Rent::default().minimum_balance(data.len());

    Account {
        owner: token_program,
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
    let bump =
        magicblock_core::token_programs::try_derive_eata_address_and_bump(
            owner, mint,
        )
        .expect("derive eATA")
        .1;
    let eata_account: Account = EphemeralAta {
        owner: *owner,
        mint: *mint,
        amount,
        bump,
    }
    .into();

    let account_owner = if delegate {
        dlp_api::id()
    } else {
        EATA_PROGRAM_ID
    };

    Account {
        owner: account_owner,
        data: eata_account.data,
        lamports: eata_account.lamports,
        ..Default::default()
    }
}

// Reuse EphemeralAta definition from magicblock_core::token_programs
