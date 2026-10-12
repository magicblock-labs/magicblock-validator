use magicblock_core::token_programs::{TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID};
use solana_account::Account;
use solana_program::{program_option::COption, program_pack::Pack, rent::Rent};
use solana_pubkey::Pubkey;
use spl_token::state::{Account as SplAccount, AccountState};
use spl_token_2022::{
    extension::{
        BaseStateWithExtensionsMut, ExtensionType, StateWithExtensionsMut, set_account_type,
    },
    state::Account as Token2022Account,
};

/// Creates a test ATA (Associated Token Account) with initialized state and zero balance.
///
/// # Arguments
/// * `owner` - The public key of the account owner
/// * `mint` - The public key of the token mint
pub(crate) fn create_ata_account(owner: &Pubkey, mint: &Pubkey) -> Account {
    create_ata_account_with_token_program(owner, mint, TOKEN_PROGRAM_ID, SplAccount::LEN)
}

pub(crate) fn create_token_2022_ata_account(owner: &Pubkey, mint: &Pubkey) -> Account {
    // Default Token-2022 ATAs carry ImmutableOwner; mint-specific extensions can require more.
    create_token_2022_ata_account_with_extensions(owner, mint, &[ExtensionType::ImmutableOwner])
}

pub(crate) fn create_token_2022_ata_account_with_extensions(
    owner: &Pubkey,
    mint: &Pubkey,
    account_extensions: &[ExtensionType],
) -> Account {
    // Token-2022 account length depends on account extensions required by the mint.
    let data_len = ExtensionType::try_calculate_account_len::<Token2022Account>(account_extensions)
        .expect("calculate Token-2022 account length");
    let mut account =
        create_ata_account_with_token_program(owner, mint, TOKEN_2022_PROGRAM_ID, data_len);
    initialize_token_2022_account_extensions(&mut account.data, account_extensions);
    account
}

fn initialize_token_2022_account_extensions(data: &mut [u8], account_extensions: &[ExtensionType]) {
    if account_extensions.is_empty() {
        return;
    }
    set_account_type::<Token2022Account>(data).expect("set Token-2022 account type");
    let mut state = StateWithExtensionsMut::<Token2022Account>::unpack(data)
        .expect("unpack Token-2022 account");
    for extension_type in account_extensions {
        state
            .init_account_extension_from_type(*extension_type)
            .expect("initialize Token-2022 account extension");
    }
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
    SplAccount::pack(token_account, &mut packed).expect("pack spl token account");
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
