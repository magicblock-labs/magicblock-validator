use magicblock_core::token_programs::{
    MAGIC_ATA_CLOSE_AUTHORITY, TOKEN_PROGRAM_ID, derive_ata_with_token_program,
    is_supported_token_program, try_get_magic_ata_info, try_remap_ata_to_eata,
};
use solana_account::{AccountMode, ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program::{program_option::COption, program_pack::Pack};
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;
use solana_transaction_context::transaction::TransactionContext;
use spl_token::state::{
    Account as SplAccount, AccountState as SplAccountState, Mint as SplMint,
};
use spl_token_2022::{
    extension::{
        BaseStateWithExtensions, BaseStateWithExtensionsMut, ExtensionType,
        StateWithExtensions, StateWithExtensionsMut,
        default_account_state::DefaultAccountState,
    },
    state::{Account as Token2022Account, AccountState, Mint as Token2022Mint},
};

use crate::utils::{
    account_actions::set_account_mode,
    accounts::{
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
    },
};

const PAYER_IDX: u16 = 0;
const ATA_IDX: u16 = 1;
const MINT_IDX: u16 = 2;
const TOKEN_PROGRAM_IDX: u16 = 3;

struct TokenAccountShape {
    len: usize,
    required_extensions: Vec<ExtensionType>,
    initial_state: AccountState,
}

pub(crate) fn process_create_magic_ata(
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    wallet_owner: Pubkey,
) -> Result<(), InstructionError> {
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    if !ix_ctx.is_instruction_account_signer(PAYER_IDX)? {
        return Err(InstructionError::MissingRequiredSignature);
    }

    let ata_pubkey =
        *get_instruction_pubkey_with_idx(transaction_context, ATA_IDX)?;
    let mint = *get_instruction_pubkey_with_idx(transaction_context, MINT_IDX)?;
    let token_program = *get_instruction_pubkey_with_idx(
        transaction_context,
        TOKEN_PROGRAM_IDX,
    )?;

    // Default owner is reserved as the MagicContext legacy-decode sentinel.
    if wallet_owner == Pubkey::default() {
        return Err(InstructionError::InvalidArgument);
    }
    if !is_supported_token_program(&token_program) {
        return Err(InstructionError::UnsupportedProgramId);
    }
    if is_native_mint(&mint) {
        return Err(InstructionError::InvalidArgument);
    }
    let expected_ata =
        derive_ata_with_token_program(&wallet_owner, &mint, &token_program);
    if ata_pubkey != expected_ata {
        return Err(InstructionError::InvalidSeeds);
    }

    let ata = get_instruction_account_with_idx(transaction_context, ATA_IDX)?;
    {
        let ata_shared = ata.borrow()?;
        if !is_empty_system_account(&ata_shared) {
            return if matches_existing_ata(
                &ata_pubkey,
                &ata_shared,
                &wallet_owner,
                &mint,
                &token_program,
            ) {
                Ok(())
            } else {
                Err(InstructionError::InvalidAccountData)
            };
        }
    }

    let mint_acc =
        get_instruction_account_with_idx(transaction_context, MINT_IDX)?;
    let token_account_shape = {
        let mint_acc = mint_acc.borrow()?;
        if mint_acc.owner() != &token_program {
            return Err(InstructionError::InvalidAccountOwner);
        }
        token_account_shape(mint_acc.data(), &token_program)?
    };

    let mut acc = ata.borrow_mut()?;
    acc.set_lamports(0);
    acc.set_owner(token_program);
    acc.resize(token_account_shape.len, 0);
    initialize_token_data(
        acc.data_as_mut_slice(),
        &token_program,
        wallet_owner,
        mint,
        &token_account_shape.required_extensions,
        token_account_shape.initial_state,
    )?;
    set_account_mode(invoke_context, &mut acc, AccountMode::Magic)?;

    ic_msg!(
        invoke_context,
        "Created Magic ATA {} for owner {} mint {}",
        ata_pubkey,
        wallet_owner,
        mint
    );
    Ok(())
}

fn is_empty_system_account(
    account: &solana_account::AccountSharedData,
) -> bool {
    account.lamports() == 0
        && account.owner() == &system_program::ID
        && account.data().is_empty()
}

fn is_native_mint(mint: &Pubkey) -> bool {
    *mint == spl_token::native_mint::id()
        || *mint == spl_token_2022::native_mint::id()
}

fn token_account_shape(
    mint_data: &[u8],
    token_program: &Pubkey,
) -> Result<TokenAccountShape, InstructionError> {
    if *token_program == TOKEN_PROGRAM_ID {
        SplMint::unpack(mint_data)
            .map_err(|_| InstructionError::InvalidAccountData)?;
        Ok(TokenAccountShape {
            len: SplAccount::LEN,
            required_extensions: Vec::new(),
            initial_state: AccountState::Initialized,
        })
    } else {
        let mint = StateWithExtensions::<Token2022Mint>::unpack(mint_data)
            .map_err(|_| InstructionError::InvalidAccountData)?;
        let mint_extensions = mint
            .get_extension_types()
            .map_err(|_| InstructionError::InvalidAccountData)?;
        let initial_state =
            if mint_extensions.contains(&ExtensionType::DefaultAccountState) {
                let default_state = mint
                    .get_extension::<DefaultAccountState>()
                    .map_err(|_| InstructionError::InvalidAccountData)?;
                AccountState::try_from(default_state.state)
                    .map_err(|_| InstructionError::InvalidAccountData)?
            } else {
                AccountState::Initialized
            };
        let mut required_extensions =
            ExtensionType::get_required_init_account_extensions(
                &mint_extensions,
            );
        if required_extensions.iter().any(|extension| {
            !is_supported_magic_ata_account_extension(*extension)
        }) {
            return Err(InstructionError::InvalidAccountData);
        }
        if !required_extensions.contains(&ExtensionType::ImmutableOwner) {
            required_extensions.push(ExtensionType::ImmutableOwner);
        }
        let len = ExtensionType::try_calculate_account_len::<Token2022Account>(
            &required_extensions,
        )
        .map_err(|_| InstructionError::InvalidAccountData)?;
        Ok(TokenAccountShape {
            len,
            required_extensions,
            initial_state,
        })
    }
}

fn is_supported_magic_ata_account_extension(extension: ExtensionType) -> bool {
    matches!(
        extension,
        ExtensionType::ImmutableOwner
            | ExtensionType::NonTransferableAccount
            | ExtensionType::PausableAccount
    )
}

fn initialize_token_data(
    data: &mut [u8],
    token_program: &Pubkey,
    wallet_owner: Pubkey,
    mint: Pubkey,
    required_extensions: &[ExtensionType],
    initial_state: AccountState,
) -> Result<(), InstructionError> {
    if *token_program == TOKEN_PROGRAM_ID {
        // Both token crates define their own AccountState enum with the same
        // u8 layout, so the shared initial state is carried over by value.
        let state = SplAccountState::try_from(initial_state as u8)
            .map_err(|_| InstructionError::InvalidAccountData)?;
        let account = SplAccount {
            mint,
            owner: wallet_owner,
            amount: 0,
            delegate: COption::None,
            state,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::Some(MAGIC_ATA_CLOSE_AUTHORITY),
        };
        SplAccount::pack(account, data)
            .map_err(|_| InstructionError::InvalidAccountData)
    } else {
        let mut state =
            StateWithExtensionsMut::<Token2022Account>::unpack_uninitialized(
                data,
            )
            .map_err(|_| InstructionError::InvalidAccountData)?;
        for extension in required_extensions {
            state
                .init_account_extension_from_type(*extension)
                .map_err(|_| InstructionError::InvalidAccountData)?;
        }
        state.base = Token2022Account {
            mint,
            owner: wallet_owner,
            amount: 0,
            delegate: COption::None,
            state: initial_state,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::Some(MAGIC_ATA_CLOSE_AUTHORITY),
        };
        state.pack_base();
        state
            .init_account_type()
            .map_err(|_| InstructionError::InvalidAccountData)
    }
}

fn matches_existing_ata(
    ata_pubkey: &Pubkey,
    account: &solana_account::AccountSharedData,
    wallet_owner: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> bool {
    if account.owner() != token_program {
        return false;
    }
    try_get_magic_ata_info(ata_pubkey, account)
        .map(|info| (info.wallet_owner, info.mint))
        .or_else(|| {
            try_remap_ata_to_eata(ata_pubkey, account)
                .map(|(_, eata)| (eata.owner, eata.mint))
        })
        .is_some_and(|identity| identity == (*wallet_owner, *mint))
}

#[cfg(test)]
mod tests {
    use magicblock_core::token_programs::TOKEN_2022_PROGRAM_ID;
    use magicblock_magic_program_api::instruction::MagicBlockInstruction;
    use solana_account::{AccountSharedData, ReadableAccount, WritableAccount};
    use solana_instruction::{AccountMeta, Instruction};
    use solana_sdk_ids::{native_loader, system_program};
    use spl_token_2022::extension::{
        immutable_owner::ImmutableOwner,
        non_transferable::{NonTransferable, NonTransferableAccount},
        transfer_fee::TransferFeeConfig,
    };

    use super::*;
    use crate::test_utils::process_instruction;

    fn spl_mint_account() -> AccountSharedData {
        let mint_state = SplMint {
            mint_authority: COption::None,
            supply: 0,
            decimals: 6,
            is_initialized: true,
            freeze_authority: COption::None,
        };
        let mut account =
            AccountSharedData::new(1_000_000, SplMint::LEN, &TOKEN_PROGRAM_ID);
        SplMint::pack(mint_state, account.data_as_mut_slice()).unwrap();
        account
    }

    fn token_2022_mint_account(
        extension_types: &[ExtensionType],
    ) -> AccountSharedData {
        let len = ExtensionType::try_calculate_account_len::<Token2022Mint>(
            extension_types,
        )
        .unwrap();
        let mut account =
            AccountSharedData::new(1_000_000, len, &TOKEN_2022_PROGRAM_ID);
        let mut state =
            StateWithExtensionsMut::<Token2022Mint>::unpack_uninitialized(
                account.data_as_mut_slice(),
            )
            .unwrap();
        for extension_type in extension_types {
            match extension_type {
                ExtensionType::DefaultAccountState => {
                    let extension = state
                        .init_extension::<DefaultAccountState>(false)
                        .unwrap();
                    extension.state = AccountState::Frozen.into();
                }
                ExtensionType::NonTransferable => {
                    state.init_extension::<NonTransferable>(false).unwrap();
                }
                ExtensionType::TransferFeeConfig => {
                    state.init_extension::<TransferFeeConfig>(false).unwrap();
                }
                _ => panic!("unsupported test extension"),
            }
        }
        state.base = Token2022Mint {
            mint_authority: COption::None,
            supply: 0,
            decimals: 6,
            is_initialized: true,
            freeze_authority: COption::None,
        };
        state.pack_base();
        state.init_account_type().unwrap();
        account
    }

    #[test]
    fn create_magic_ata_initializes_local_token_account_shape() {
        let payer = Pubkey::new_unique();
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata = derive_ata_with_token_program(
            &wallet_owner,
            &mint,
            &TOKEN_PROGRAM_ID,
        );

        let ix = Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::CreateMagicAta { wallet_owner },
            vec![
                AccountMeta::new(payer, true),
                AccountMeta::new(ata, false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            ],
        );
        let accounts = process_instruction(
            &ix.data,
            vec![
                (
                    payer,
                    AccountSharedData::new(1_000_000, 0, &system_program::id()),
                ),
                (ata, AccountSharedData::new(0, 0, &system_program::id())),
                (mint, spl_mint_account()),
                (
                    TOKEN_PROGRAM_ID,
                    AccountSharedData::new(1, 0, &native_loader::id()),
                ),
            ],
            ix.accounts,
            Ok(()),
        );

        let ata_after = &accounts[1];
        assert_eq!(ata_after.owner(), &TOKEN_PROGRAM_ID);
        assert!(ata_after.is(AccountMode::Magic));
        assert_eq!(ata_after.slot(), 0);

        let token_account = SplAccount::unpack(ata_after.data()).unwrap();
        assert_eq!(token_account.mint, mint);
        assert_eq!(token_account.owner, wallet_owner);
        assert_eq!(token_account.amount, 0);
        assert_eq!(token_account.delegate, COption::None);
        assert_eq!(token_account.state, SplAccountState::Initialized);
        assert_eq!(token_account.is_native, COption::None);
        assert_eq!(token_account.delegated_amount, 0);
        assert_eq!(
            token_account.close_authority,
            COption::Some(MAGIC_ATA_CLOSE_AUTHORITY)
        );
    }

    #[test]
    fn create_magic_token_2022_ata_initializes_local_token_account_shape() {
        let payer = Pubkey::new_unique();
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata = derive_ata_with_token_program(
            &wallet_owner,
            &mint,
            &TOKEN_2022_PROGRAM_ID,
        );

        let ix = Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::CreateMagicAta { wallet_owner },
            vec![
                AccountMeta::new(payer, true),
                AccountMeta::new(ata, false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
            ],
        );
        let accounts = process_instruction(
            &ix.data,
            vec![
                (
                    payer,
                    AccountSharedData::new(1_000_000, 0, &system_program::id()),
                ),
                (ata, AccountSharedData::new(0, 0, &system_program::id())),
                (mint, token_2022_mint_account(&[])),
                (
                    TOKEN_2022_PROGRAM_ID,
                    AccountSharedData::new(1, 0, &native_loader::id()),
                ),
            ],
            ix.accounts,
            Ok(()),
        );

        let ata_after = &accounts[1];
        assert_eq!(ata_after.owner(), &TOKEN_2022_PROGRAM_ID);
        let expected_len = ExtensionType::try_calculate_account_len::<
            Token2022Account,
        >(&[ExtensionType::ImmutableOwner])
        .unwrap();
        assert_eq!(ata_after.data().len(), expected_len);
        let token_account =
            StateWithExtensions::<Token2022Account>::unpack(ata_after.data())
                .unwrap();
        assert_eq!(token_account.base.mint, mint);
        assert_eq!(token_account.base.owner, wallet_owner);
        assert_eq!(token_account.base.amount, 0);
        assert_eq!(
            token_account.base.close_authority,
            COption::Some(MAGIC_ATA_CLOSE_AUTHORITY)
        );
        assert!(token_account.get_extension::<ImmutableOwner>().is_ok());

        let info = try_get_magic_ata_info(&ata, ata_after).unwrap();
        assert_eq!(info.token_program, TOKEN_2022_PROGRAM_ID);
        assert_eq!(info.wallet_owner, wallet_owner);
        assert_eq!(info.mint, mint);
    }

    #[test]
    fn create_magic_token_2022_ata_initializes_required_extensions() {
        let payer = Pubkey::new_unique();
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata = derive_ata_with_token_program(
            &wallet_owner,
            &mint,
            &TOKEN_2022_PROGRAM_ID,
        );

        let ix = Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::CreateMagicAta { wallet_owner },
            vec![
                AccountMeta::new(payer, true),
                AccountMeta::new(ata, false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
            ],
        );
        let accounts = process_instruction(
            &ix.data,
            vec![
                (
                    payer,
                    AccountSharedData::new(1_000_000, 0, &system_program::id()),
                ),
                (ata, AccountSharedData::new(0, 0, &system_program::id())),
                (
                    mint,
                    token_2022_mint_account(&[ExtensionType::NonTransferable]),
                ),
                (
                    TOKEN_2022_PROGRAM_ID,
                    AccountSharedData::new(1, 0, &native_loader::id()),
                ),
            ],
            ix.accounts,
            Ok(()),
        );

        let expected_extensions = [
            ExtensionType::NonTransferableAccount,
            ExtensionType::ImmutableOwner,
        ];
        let expected_len = ExtensionType::try_calculate_account_len::<
            Token2022Account,
        >(&expected_extensions)
        .unwrap();
        let ata_after = &accounts[1];
        assert_eq!(ata_after.data().len(), expected_len);
        let token_account =
            StateWithExtensions::<Token2022Account>::unpack(ata_after.data())
                .unwrap();
        assert!(
            token_account
                .get_extension::<NonTransferableAccount>()
                .is_ok()
        );
        assert!(token_account.get_extension::<ImmutableOwner>().is_ok());
        assert_eq!(
            token_account.base.close_authority,
            COption::Some(MAGIC_ATA_CLOSE_AUTHORITY)
        );
        assert!(try_get_magic_ata_info(&ata, ata_after).is_some());
    }

    #[test]
    fn create_magic_token_2022_ata_rejects_stateful_account_extensions() {
        let payer = Pubkey::new_unique();
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata = derive_ata_with_token_program(
            &wallet_owner,
            &mint,
            &TOKEN_2022_PROGRAM_ID,
        );

        let ix = Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::CreateMagicAta { wallet_owner },
            vec![
                AccountMeta::new(payer, true),
                AccountMeta::new(ata, false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
            ],
        );

        process_instruction(
            &ix.data,
            vec![
                (
                    payer,
                    AccountSharedData::new(1_000_000, 0, &system_program::id()),
                ),
                (ata, AccountSharedData::new(0, 0, &system_program::id())),
                (
                    mint,
                    token_2022_mint_account(&[
                        ExtensionType::TransferFeeConfig,
                    ]),
                ),
                (
                    TOKEN_2022_PROGRAM_ID,
                    AccountSharedData::new(1, 0, &native_loader::id()),
                ),
            ],
            ix.accounts,
            Err(InstructionError::InvalidAccountData),
        );
    }

    #[test]
    fn create_magic_token_2022_ata_honors_default_account_state() {
        let payer = Pubkey::new_unique();
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata = derive_ata_with_token_program(
            &wallet_owner,
            &mint,
            &TOKEN_2022_PROGRAM_ID,
        );

        let ix = Instruction::new_with_bincode(
            crate::id(),
            &MagicBlockInstruction::CreateMagicAta { wallet_owner },
            vec![
                AccountMeta::new(payer, true),
                AccountMeta::new(ata, false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
            ],
        );
        let accounts = process_instruction(
            &ix.data,
            vec![
                (
                    payer,
                    AccountSharedData::new(1_000_000, 0, &system_program::id()),
                ),
                (ata, AccountSharedData::new(0, 0, &system_program::id())),
                (
                    mint,
                    token_2022_mint_account(&[
                        ExtensionType::DefaultAccountState,
                    ]),
                ),
                (
                    TOKEN_2022_PROGRAM_ID,
                    AccountSharedData::new(1, 0, &native_loader::id()),
                ),
            ],
            ix.accounts,
            Ok(()),
        );

        let ata_after = &accounts[1];
        let token_account =
            StateWithExtensions::<Token2022Account>::unpack(ata_after.data())
                .unwrap();
        assert_eq!(token_account.base.state, AccountState::Frozen);
        assert!(try_get_magic_ata_info(&ata, ata_after).is_some());
    }
}
