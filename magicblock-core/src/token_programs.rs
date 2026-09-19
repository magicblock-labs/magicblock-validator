use solana_account::{
    Account, AccountBuilder, AccountMode, AccountSharedData, ReadableAccount,
};
use solana_program::{program_option::COption, program_pack::Pack, rent::Rent};
use solana_pubkey::{Pubkey, pubkey};
use spl_token::state::Account as SplAccount;
use spl_token_2022::{
    extension::{StateWithExtensions, StateWithExtensionsMut},
    state::Account as Token2022Account,
};

// Shared program IDs and helper functions for SPL Token, Associated Token, and eATA programs.

// Token Program ID (Tokenkeg...)
pub const TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

// Token-2022 Program ID (Tokenz...)
pub const TOKEN_2022_PROGRAM_ID: Pubkey =
    pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

// Associated Token Account Program ID (ATokenG...)
pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

// Enhanced ATA (eATA) Program ID (SPLxh1...)
pub const EATA_PROGRAM_ID: Pubkey =
    pubkey!("SPLxh1LVZzEkX99H6rqYizhytLWPZVV296zyYDPagv2");

// Marker written into a Magic ATA's close authority. It is a data
// marker only, never a signer grant: the sysvar cannot sign, so these
// accounts are closed exclusively via the Magic Program.
pub const MAGIC_ATA_CLOSE_AUTHORITY: Pubkey = solana_program::sysvar::rent::ID;

pub const EPHEMERAL_ATA_LEN: usize = 80;
const LEGACY_EPHEMERAL_ATA_LEN: usize = 72;

/// A validator-created ER-only token account at the canonical ATA address,
/// letting a wallet receive tokens before its ATA exists anywhere.
/// It remains locally authoritative, even when empty, until explicitly closed.
/// Chainlink must not replace it with a base-chain delegation while it exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MagicAtaInfo {
    pub ata_pubkey: Pubkey,
    pub token_program: Pubkey,
    pub wallet_owner: Pubkey,
    pub mint: Pubkey,
    pub amount: u64,
}

struct TokenAccountCommon {
    mint: Pubkey,
    owner: Pubkey,
    amount: u64,
    is_native: COption<u64>,
    close_authority: COption<Pubkey>,
}

/// Private WSOL is represented locally by the token amount that maps to eATA,
/// not by claimable lamports on the projected ATA.
/// Returns `None` when token-program data is malformed and the clone should be
/// rejected.
pub fn normalize_native_token_account_for_local_clone(
    account: AccountBuilder,
) -> Option<AccountBuilder> {
    let normalized = if account.read().owner() == TOKEN_PROGRAM_ID {
        normalize_legacy_native_token_account(account.read())
    } else if account.read().owner() == TOKEN_2022_PROGRAM_ID {
        normalize_token_2022_native_token_account(account.read())
    } else {
        NativeTokenNormalization::NotNative
    };

    match normalized {
        NativeTokenNormalization::NotNative => Some(account),
        NativeTokenNormalization::Invalid => None,
        NativeTokenNormalization::Normalized {
            data,
            rent_exempt_reserve,
        } => Some(account.data(data).lamports(rent_exempt_reserve)),
    }
}

/// Projected ATAs are virtual views over eATA state. They must not be locally
/// closeable; settlement owns materialization and closure on the base layer.
pub fn normalize_projected_token_account_for_local_clone(
    account: AccountBuilder,
) -> Option<AccountBuilder> {
    if account.read().owner() == TOKEN_PROGRAM_ID {
        normalize_legacy_projected_token_account(account)
    } else if account.read().owner() == TOKEN_2022_PROGRAM_ID {
        normalize_token_2022_projected_token_account(account)
    } else {
        Some(account)
    }
}

enum NativeTokenNormalization {
    NotNative,
    Normalized {
        data: Vec<u8>,
        rent_exempt_reserve: u64,
    },
    Invalid,
}

fn normalize_legacy_native_token_account(
    account: &solana_account::OwnedAccount,
) -> NativeTokenNormalization {
    let Ok(mut token_account) = SplAccount::unpack(account.data()) else {
        return NativeTokenNormalization::Invalid;
    };
    if token_account.mint != spl_token::native_mint::id() {
        return NativeTokenNormalization::NotNative;
    }

    let COption::Some(rent_exempt_reserve) = token_account.is_native else {
        return NativeTokenNormalization::NotNative;
    };

    token_account.is_native = COption::None;
    token_account.close_authority = COption::Some(Pubkey::default());
    let mut data = account.data().to_vec();
    if SplAccount::pack(token_account, &mut data).is_err() {
        return NativeTokenNormalization::Invalid;
    }
    NativeTokenNormalization::Normalized {
        data,
        rent_exempt_reserve,
    }
}

fn normalize_legacy_projected_token_account(
    account: AccountBuilder,
) -> Option<AccountBuilder> {
    let Ok(mut token_account) = SplAccount::unpack(account.read().data())
    else {
        return None;
    };
    let rent_exempt_reserve = if token_account.mint
        == spl_token::native_mint::id()
        && let COption::Some(rent_exempt_reserve) = token_account.is_native
    {
        token_account.is_native = COption::None;
        Some(rent_exempt_reserve)
    } else {
        None
    };
    token_account.close_authority = COption::Some(Pubkey::default());
    let mut data = account.read().data().to_vec();
    SplAccount::pack(token_account, &mut data).ok()?;
    let account = account.data(data);
    Some(match rent_exempt_reserve {
        Some(rent_exempt_reserve) => account.lamports(rent_exempt_reserve),
        None => account,
    })
}

fn normalize_token_2022_native_token_account(
    account: &solana_account::OwnedAccount,
) -> NativeTokenNormalization {
    let mut data = account.data().to_vec();
    let Ok(mut state) =
        StateWithExtensionsMut::<Token2022Account>::unpack(&mut data)
    else {
        return NativeTokenNormalization::Invalid;
    };
    if state.base.mint != spl_token_2022::native_mint::id() {
        return NativeTokenNormalization::NotNative;
    }

    let COption::Some(rent_exempt_reserve) = state.base.is_native else {
        return NativeTokenNormalization::NotNative;
    };

    state.base.is_native = COption::None;
    state.base.close_authority = COption::Some(Pubkey::default());
    state.pack_base();
    NativeTokenNormalization::Normalized {
        data,
        rent_exempt_reserve,
    }
}

fn normalize_token_2022_projected_token_account(
    account: AccountBuilder,
) -> Option<AccountBuilder> {
    let mut data = account.read().data().to_vec();
    let rent_exempt_reserve = {
        let Ok(mut state) =
            StateWithExtensionsMut::<Token2022Account>::unpack(&mut data)
        else {
            return None;
        };
        let rent_exempt_reserve =
            if state.base.mint == spl_token_2022::native_mint::id() {
                match state.base.is_native {
                    COption::Some(rent_exempt_reserve) => {
                        state.base.is_native = COption::None;
                        Some(rent_exempt_reserve)
                    }
                    COption::None => None,
                }
            } else {
                None
            };
        state.base.close_authority = COption::Some(Pubkey::default());
        state.pack_base();
        rent_exempt_reserve
    };
    let account = account.data(data);
    Some(match rent_exempt_reserve {
        Some(rent_exempt_reserve) => account.lamports(rent_exempt_reserve),
        None => account,
    })
}

/// Derives the standard Associated Token Account (ATA) address for the given wallet owner and token mint.
///
/// # Arguments
/// * `owner` - The public key of the account owner
/// * `mint` - The public key of the token mint
///
/// # Returns
/// The derived ATA address as `Pubkey`.
pub fn derive_ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    derive_ata_with_token_program(owner, mint, &TOKEN_PROGRAM_ID)
}

pub fn derive_ata_with_token_program(
    owner: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
    .0
}

/// Attempts to derive the ATA address for the given wallet owner and token mint, returning the address and bump.
///
/// # Arguments
/// * `owner` - The public key of the account owner
/// * `mint` - The public key of the token mint
///
/// # Returns
/// `Option<(Pubkey, u8)>` — `Some((address, bump))` if derivation succeeds, `None` otherwise.
pub fn try_derive_ata_address_and_bump(
    owner: &Pubkey,
    mint: &Pubkey,
) -> Option<(Pubkey, u8)> {
    try_derive_ata_address_and_bump_with_token_program(
        owner,
        mint,
        &TOKEN_PROGRAM_ID,
    )
}

pub fn try_derive_ata_address_and_bump_with_token_program(
    owner: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Option<(Pubkey, u8)> {
    Pubkey::try_find_program_address(
        &[owner.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupportedAtaPubkeys {
    pub legacy: Option<Pubkey>,
    pub token_2022: Option<Pubkey>,
}

impl SupportedAtaPubkeys {
    pub fn token_2022_first(&self) -> [Option<Pubkey>; 2] {
        [self.token_2022, self.legacy]
    }

    pub fn contains(&self, pubkey: &Pubkey) -> bool {
        self.legacy.as_ref() == Some(pubkey)
            || self.token_2022.as_ref() == Some(pubkey)
    }
}

pub fn try_derive_supported_ata_pubkeys(
    owner: &Pubkey,
    mint: &Pubkey,
) -> SupportedAtaPubkeys {
    SupportedAtaPubkeys {
        legacy: try_derive_ata_address_and_bump_with_token_program(
            owner,
            mint,
            &TOKEN_PROGRAM_ID,
        )
        .map(|(pubkey, _)| pubkey),
        token_2022: try_derive_ata_address_and_bump_with_token_program(
            owner,
            mint,
            &TOKEN_2022_PROGRAM_ID,
        )
        .map(|(pubkey, _)| pubkey),
    }
}

/// Derives the Enhanced Associated Token Account (eATA) Program Derived Address (PDA) for the given wallet owner and token mint.
///
/// # Arguments
/// * `owner` - The public key of the account owner
/// * `mint` - The public key of the token mint
///
/// # Returns
/// The derived eATA PDA as `Pubkey`.
pub fn derive_eata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), mint.as_ref()],
        &EATA_PROGRAM_ID,
    )
    .0
}

/// Attempts to derive the eATA PDA for the given wallet owner and token mint, returning the address and bump.
///
/// # Arguments
/// * `owner` - The public key of the account owner
/// * `mint` - The public key of the token mint
///
/// # Returns
/// `Option<(Pubkey, u8)>` — `Some((address, bump))` if derivation succeeds, `None` otherwise.
pub fn try_derive_eata_address_and_bump(
    owner: &Pubkey,
    mint: &Pubkey,
) -> Option<(Pubkey, u8)> {
    Pubkey::try_find_program_address(
        &[owner.as_ref(), mint.as_ref()],
        &EATA_PROGRAM_ID,
    )
}

// ---------------- ATA inspection helpers ----------------

/// Information about an Associated Token Account (ATA)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtaInfo {
    pub mint: Pubkey,
    pub owner: Pubkey,
}

/// Returns Some(AtaInfo) if the given account is an Associated Token Account (ATA)
/// for the mint/owner contained in its SPL Token account data.
/// Supports both spl-token and spl-token-2022 program owners.
pub fn is_ata(
    account_pubkey: &Pubkey,
    token_program_owner: Pubkey,
    data: &[u8],
) -> Option<AtaInfo> {
    // The account must be owned by the SPL Token program (legacy) or Token-2022
    let is_spl_token = token_program_owner == spl_token::id();
    let is_token_2022 = token_program_owner == TOKEN_2022_PROGRAM_ID;
    if !(is_spl_token || is_token_2022) {
        return None;
    }

    // Parse the token account data to extract mint and token owner
    // Layout (at least the first 64 bytes):
    // 0..32  -> mint Pubkey
    // 32..64 -> owner Pubkey (the wallet the ATA belongs to)
    if data.len() < 64 {
        return None;
    }

    let mint = Pubkey::new_from_array(match data[0..32].try_into() {
        Ok(a) => a,
        Err(_) => return None,
    });
    let wallet_owner = Pubkey::new_from_array(match data[32..64].try_into() {
        Ok(a) => a,
        Err(_) => return None,
    });

    // Seeds per SPL ATA derivation: [wallet_owner, token_program_id, mint]
    let (derived, _bump) = Pubkey::find_program_address(
        &[
            wallet_owner.as_ref(),
            token_program_owner.as_ref(),
            mint.as_ref(),
        ],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    );

    if derived == *account_pubkey {
        Some(AtaInfo {
            mint,
            owner: wallet_owner,
        })
    } else {
        None
    }
}

/// Return the eata pubkey and EphemeralAta
pub fn try_remap_ata_to_eata(
    pubkey: &Pubkey,
    account: &AccountSharedData,
) -> Option<(Pubkey, EphemeralAta)> {
    let token_program_owner = account.owner();
    let is_spl_token = *token_program_owner == TOKEN_PROGRAM_ID;
    let is_token_2022 = *token_program_owner == TOKEN_2022_PROGRAM_ID;
    if !(is_spl_token || is_token_2022) || !account.is(AccountMode::Delegated) {
        return None;
    }

    let data = account.data();
    if data.len() < 72 {
        return None;
    }

    let mint = Pubkey::new_from_array(data[0..32].try_into().ok()?);
    let owner = Pubkey::new_from_array(data[32..64].try_into().ok()?);
    let amount = u64::from_le_bytes(data[64..72].try_into().ok()?);

    let (eata_pubkey, bump) = try_derive_eata_address_and_bump(&owner, &mint)?;
    let ata = derive_ata_with_token_program(&owner, &mint, token_program_owner);
    if ata != *pubkey {
        return None;
    }

    let eata = EphemeralAta {
        owner,
        mint,
        amount,
        bump,
    };

    Some((eata_pubkey, eata))
}

/// Recognizes a local Magic-mode ATA by its token layout, address and close marker.
pub fn try_get_magic_ata_info(
    pubkey: &Pubkey,
    account: &AccountSharedData,
) -> Option<MagicAtaInfo> {
    if !account.is(AccountMode::Magic) {
        return None;
    }

    let token_program = account.owner();
    let token_account =
        parse_token_account_for_magic_ata(token_program, account.data())?;
    if token_account.close_authority != COption::Some(MAGIC_ATA_CLOSE_AUTHORITY)
        || token_account.is_native.is_some()
    {
        return None;
    }
    // Degenerate default keys never classify as a Magic ATA.
    if token_account.owner == Pubkey::default()
        || token_account.mint == Pubkey::default()
    {
        return None;
    }

    let expected_ata = derive_ata_with_token_program(
        &token_account.owner,
        &token_account.mint,
        token_program,
    );
    if expected_ata != *pubkey {
        return None;
    }

    Some(MagicAtaInfo {
        ata_pubkey: *pubkey,
        token_program: *token_program,
        wallet_owner: token_account.owner,
        mint: token_account.mint,
        amount: token_account.amount,
    })
}

pub fn is_supported_token_program(token_program: &Pubkey) -> bool {
    *token_program == TOKEN_PROGRAM_ID
        || *token_program == TOKEN_2022_PROGRAM_ID
}

fn parse_token_account_for_magic_ata(
    token_program: &Pubkey,
    data: &[u8],
) -> Option<TokenAccountCommon> {
    if *token_program == TOKEN_PROGRAM_ID {
        let account = SplAccount::unpack(data).ok()?;
        Some(TokenAccountCommon {
            mint: account.mint,
            owner: account.owner,
            amount: account.amount,
            is_native: account.is_native,
            close_authority: account.close_authority,
        })
    } else if *token_program == TOKEN_2022_PROGRAM_ID {
        let account = StateWithExtensions::<Token2022Account>::unpack(data)
            .ok()?
            .base;
        Some(TokenAccountCommon {
            mint: account.mint,
            owner: account.owner,
            amount: account.amount,
            is_native: account.is_native,
            close_authority: account.close_authority,
        })
    } else {
        None
    }
}

// ---------------- eATA -> ATA projection helpers ----------------

/// Minimal ephemeral representation of an SPL token account used to build
/// a real AccountSharedData with correct layout and rent.
#[repr(C)]
pub struct EphemeralAta {
    /// The owner (wallet) this ATA belongs to
    pub owner: Pubkey,
    /// The mint associated with this account
    pub mint: Pubkey,
    /// The amount of tokens this account holds.
    pub amount: u64,
    /// The bump of the eATA PDA.
    pub bump: u8,
}

impl EphemeralAta {
    pub fn try_from_account_data(data: &[u8]) -> Option<Self> {
        let owner = Pubkey::new_from_array(data.get(0..32)?.try_into().ok()?);
        let mint = Pubkey::new_from_array(data.get(32..64)?.try_into().ok()?);
        if mint == Pubkey::default() {
            return None;
        }
        let amount = u64::from_le_bytes(data.get(64..72)?.try_into().ok()?);
        let bump = match data.len() {
            EPHEMERAL_ATA_LEN => data[72],
            LEGACY_EPHEMERAL_ATA_LEN => {
                try_derive_eata_address_and_bump(&owner, &mint)?.1
            }
            _ => return None,
        };

        Some(Self {
            owner,
            mint,
            amount,
            bump,
        })
    }

    pub fn project_into_ata_account(
        &self,
        ata_account: AccountBuilder,
    ) -> Option<AccountBuilder> {
        let token_program_owner = ata_account.read().owner();
        let is_spl_token = token_program_owner == TOKEN_PROGRAM_ID;
        let is_token_2022 = token_program_owner == TOKEN_2022_PROGRAM_ID;
        if !(is_spl_token || is_token_2022) {
            return None;
        }

        let data = ata_account.read().data();
        if data.len() < 72 {
            return None;
        }
        if &data[0..32] != self.mint.as_ref()
            || &data[32..64] != self.owner.as_ref()
        {
            return None;
        }

        let mut data = data.to_vec();
        data[64..72].copy_from_slice(&self.amount.to_le_bytes());
        normalize_projected_token_account_for_local_clone(
            ata_account.data(data),
        )
    }
}

impl From<EphemeralAta> for Account {
    fn from(val: EphemeralAta) -> Self {
        let mut data = Vec::with_capacity(EPHEMERAL_ATA_LEN);
        data.extend_from_slice(val.owner.as_ref());
        data.extend_from_slice(val.mint.as_ref());
        data.extend_from_slice(&val.amount.to_le_bytes());
        data.push(val.bump);
        data.extend_from_slice(&[0; 7]);

        Account {
            lamports: Rent::default().minimum_balance(data.len()),
            data,
            owner: EATA_PROGRAM_ID,
            executable: false,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use solana_account::WritableAccount;
    use spl_token::state::AccountState;

    use super::*;

    #[test]
    fn project_non_native_ata_is_uncloseable() {
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let close_authority = Pubkey::new_unique();
        let amount = 100_000_000;
        let rent_exempt_reserve =
            Rent::default().minimum_balance(SplAccount::LEN);
        let token_account = SplAccount {
            mint,
            owner: wallet_owner,
            amount: 0,
            delegate: COption::None,
            state: AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::Some(close_authority),
        };

        let mut data = vec![0u8; SplAccount::LEN];
        SplAccount::pack(token_account, &mut data).unwrap();
        let base_ata = AccountBuilder::from(Account {
            owner: TOKEN_PROGRAM_ID,
            data,
            lamports: rent_exempt_reserve,
            executable: false,
            ..Default::default()
        });

        let eata = EphemeralAta {
            owner: wallet_owner,
            mint,
            amount,
            bump: 0,
        };

        let projected = eata
            .project_into_ata_account(base_ata)
            .expect("ATA should project");
        let projected = projected.read();
        assert_eq!(projected.lamports(), rent_exempt_reserve);

        let projected_token =
            SplAccount::unpack(projected.data()).expect("unpack projected");
        assert_eq!(projected_token.amount, amount);
        assert_eq!(projected_token.is_native, COption::None);
        assert_eq!(
            projected_token.close_authority,
            COption::Some(Pubkey::default())
        );
    }

    #[test]
    fn project_native_ata_uses_data_only_local_amount() {
        let wallet_owner = Pubkey::new_unique();
        let mint = spl_token::native_mint::id();
        let amount = 100_000_000;
        let rent_exempt_reserve =
            Rent::default().minimum_balance(SplAccount::LEN);
        let token_account = SplAccount {
            mint,
            owner: wallet_owner,
            amount: 0,
            delegate: COption::None,
            state: AccountState::Initialized,
            is_native: COption::Some(rent_exempt_reserve),
            delegated_amount: 0,
            close_authority: COption::None,
        };

        let mut data = vec![0u8; SplAccount::LEN];
        SplAccount::pack(token_account, &mut data).unwrap();
        let base_ata = AccountBuilder::from(Account {
            owner: TOKEN_PROGRAM_ID,
            data,
            lamports: rent_exempt_reserve,
            executable: false,
            ..Default::default()
        });

        let eata = EphemeralAta {
            owner: wallet_owner,
            mint,
            amount,
            bump: 0,
        };

        let projected = eata
            .project_into_ata_account(base_ata)
            .expect("native ATA should project");
        let projected = projected.read();
        assert_eq!(projected.lamports(), rent_exempt_reserve);

        let projected_token =
            SplAccount::unpack(projected.data()).expect("unpack projected");
        assert_eq!(projected_token.amount, amount);
        assert_eq!(projected_token.is_native, COption::None);
        assert_eq!(
            projected_token.close_authority,
            COption::Some(Pubkey::default())
        );
    }

    #[test]
    fn project_token_2022_non_native_ata_is_uncloseable() {
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let close_authority = Pubkey::new_unique();
        let amount = 100_000_000;
        let rent_exempt_reserve =
            Rent::default().minimum_balance(Token2022Account::LEN);
        let token_account = Token2022Account {
            mint,
            owner: wallet_owner,
            amount: 0,
            delegate: COption::None,
            state: spl_token_2022::state::AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::Some(close_authority),
        };

        let mut data = vec![0u8; Token2022Account::LEN];
        Token2022Account::pack(token_account, &mut data).unwrap();
        let base_ata = AccountBuilder::from(Account {
            owner: TOKEN_2022_PROGRAM_ID,
            data,
            lamports: rent_exempt_reserve,
            executable: false,
            ..Default::default()
        });

        let eata = EphemeralAta {
            owner: wallet_owner,
            mint,
            amount,
            bump: 0,
        };

        let projected = eata
            .project_into_ata_account(base_ata)
            .expect("Token-2022 ATA should project");
        let projected = projected.read();
        assert_eq!(projected.owner(), TOKEN_2022_PROGRAM_ID);
        assert_eq!(projected.lamports(), rent_exempt_reserve);
        assert_eq!(projected.data().len(), Token2022Account::LEN);

        let projected_token = Token2022Account::unpack(projected.data())
            .expect("unpack projected Token-2022 ATA");
        assert_eq!(projected_token.amount, amount);
        assert_eq!(projected_token.is_native, COption::None);
        assert_eq!(
            projected_token.close_authority,
            COption::Some(Pubkey::default())
        );
    }

    #[test]
    fn magic_ata_uses_rent_sysvar_close_authority() {
        let wallet_owner = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let ata = derive_ata(&wallet_owner, &mint);
        let token_account = SplAccount {
            mint,
            owner: wallet_owner,
            amount: 9,
            delegate: COption::None,
            state: AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::Some(MAGIC_ATA_CLOSE_AUTHORITY),
        };

        let mut data = vec![0u8; SplAccount::LEN];
        SplAccount::pack(token_account, &mut data).unwrap();
        let account = AccountSharedData::from(Account {
            owner: TOKEN_PROGRAM_ID,
            data,
            lamports: 0,
            executable: false,
            ..Default::default()
        });
        let account = AccountBuilder::from(account)
            .mode(AccountMode::Magic)
            .build();

        let info = try_get_magic_ata_info(&ata, &account)
            .expect("Magic ATA should be detected");
        assert_eq!(info.ata_pubkey, ata);
        assert_eq!(info.token_program, TOKEN_PROGRAM_ID);
        assert_eq!(info.wallet_owner, wallet_owner);
        assert_eq!(info.mint, mint);
        assert_eq!(info.amount, 9);

        let mut default_close_authority = account.clone();
        let mut token =
            SplAccount::unpack(default_close_authority.data()).unwrap();
        token.close_authority = COption::Some(Pubkey::default());
        SplAccount::pack(token, default_close_authority.data_as_mut_slice())
            .unwrap();
        assert!(
            try_get_magic_ata_info(&ata, &default_close_authority).is_none()
        );

        for mode in [
            AccountMode::Uninit,
            AccountMode::ReadOnly,
            AccountMode::System,
            AccountMode::Delegated,
            AccountMode::Transient,
            AccountMode::Closed,
        ] {
            let other_mode =
                AccountBuilder::from(account.clone()).mode(mode).build();
            assert!(try_get_magic_ata_info(&ata, &other_mode).is_none());
        }

        let mut wrong_owner = account.clone();
        wrong_owner.set_owner(Pubkey::new_unique());
        assert!(try_get_magic_ata_info(&ata, &wrong_owner).is_none());

        let mut native_account = account.clone();
        let mut token = SplAccount::unpack(native_account.data()).unwrap();
        token.is_native = COption::Some(0);
        SplAccount::pack(token, native_account.data_as_mut_slice()).unwrap();
        assert!(try_get_magic_ata_info(&ata, &native_account).is_none());
    }

    #[test]
    fn magic_ata_rejects_default_owner_and_mint() {
        for (wallet_owner, mint) in [
            (Pubkey::default(), Pubkey::new_unique()),
            (Pubkey::new_unique(), Pubkey::default()),
        ] {
            let ata = derive_ata(&wallet_owner, &mint);
            let token_account = SplAccount {
                mint,
                owner: wallet_owner,
                amount: 9,
                delegate: COption::None,
                state: AccountState::Initialized,
                is_native: COption::None,
                delegated_amount: 0,
                close_authority: COption::Some(MAGIC_ATA_CLOSE_AUTHORITY),
            };
            let mut data = vec![0u8; SplAccount::LEN];
            SplAccount::pack(token_account, &mut data).unwrap();
            let account = AccountSharedData::from(Account {
                owner: TOKEN_PROGRAM_ID,
                data,
                lamports: 0,
                executable: false,
                ..Default::default()
            });
            let account = AccountBuilder::from(account)
                .mode(AccountMode::Magic)
                .build();

            assert!(try_get_magic_ata_info(&ata, &account).is_none());
        }
    }
}
