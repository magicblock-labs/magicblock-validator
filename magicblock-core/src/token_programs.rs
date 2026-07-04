use serde::{Deserialize, Serialize};
use solana_account::{
    Account, AccountSharedData, ReadableAccount, WritableAccount,
};
use solana_program::{program_option::COption, program_pack::Pack, rent::Rent};
use solana_pubkey::{pubkey, Pubkey};
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
pub const RENT_PENDING_ATA_CLOSE_AUTHORITY: Pubkey =
    solana_program::sysvar::rent::ID;

pub const EPHEMERAL_ATA_LEN: usize = 80;
const LEGACY_EPHEMERAL_ATA_LEN: usize = 72;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RentPendingAtaMaterialization {
    pub ata_pubkey: Pubkey,
    pub eata_pubkey: Pubkey,
    pub token_program: Pubkey,
    pub wallet_owner: Pubkey,
    pub mint: Pubkey,
    pub token_account_data_len: u64,
    pub validator: Pubkey,
    pub delegated_payer: Pubkey,
    pub delegated_vault: Pubkey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RentPendingAtaInfo {
    pub ata_pubkey: Pubkey,
    pub eata_pubkey: Pubkey,
    pub token_program: Pubkey,
    pub wallet_owner: Pubkey,
    pub mint: Pubkey,
    pub amount: u64,
    pub token_account_data_len: u64,
}

struct ParsedTokenAccount {
    mint: Pubkey,
    owner: Pubkey,
    amount: u64,
    is_native: COption<u64>,
    close_authority: COption<Pubkey>,
}

/// Private WSOL is represented locally by the token amount that maps to eATA,
/// not by claimable lamports on the projected ATA.
/// Returns false when token-program data is malformed and a caller that already
/// rewrote projected token data should reject the clone.
pub fn normalize_native_token_account_for_local_clone(
    account: &mut AccountSharedData,
) -> bool {
    let normalized = if account.owner() == &TOKEN_PROGRAM_ID {
        normalize_legacy_native_token_account(account)
    } else if account.owner() == &TOKEN_2022_PROGRAM_ID {
        normalize_token_2022_native_token_account(account)
    } else {
        NativeTokenNormalization::NotNative
    };

    match normalized {
        NativeTokenNormalization::NotNative => true,
        NativeTokenNormalization::Invalid => false,
        NativeTokenNormalization::Normalized(rent_exempt_reserve) => {
            account.set_lamports(rent_exempt_reserve);
            true
        }
    }
}

/// Projected ATAs are virtual views over eATA state. They must not be locally
/// closeable; settlement owns materialization and closure on the base layer.
pub fn normalize_projected_token_account_for_local_clone(
    account: &mut AccountSharedData,
) -> bool {
    if account.owner() == &TOKEN_PROGRAM_ID {
        normalize_legacy_projected_token_account(account)
    } else if account.owner() == &TOKEN_2022_PROGRAM_ID {
        normalize_token_2022_projected_token_account(account)
    } else {
        true
    }
}

enum NativeTokenNormalization {
    NotNative,
    Normalized(u64),
    Invalid,
}

fn normalize_legacy_native_token_account(
    account: &mut AccountSharedData,
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
    if SplAccount::pack(token_account, account.data_as_mut_slice()).is_err() {
        return NativeTokenNormalization::Invalid;
    }
    NativeTokenNormalization::Normalized(rent_exempt_reserve)
}

fn normalize_legacy_projected_token_account(
    account: &mut AccountSharedData,
) -> bool {
    let Ok(mut token_account) = SplAccount::unpack(account.data()) else {
        return false;
    };
    if token_account.mint == spl_token::native_mint::id() {
        if let COption::Some(rent_exempt_reserve) = token_account.is_native {
            token_account.is_native = COption::None;
            account.set_lamports(rent_exempt_reserve);
        }
    }
    token_account.close_authority = COption::Some(Pubkey::default());
    SplAccount::pack(token_account, account.data_as_mut_slice()).is_ok()
}

fn normalize_token_2022_native_token_account(
    account: &mut AccountSharedData,
) -> NativeTokenNormalization {
    let Ok(mut state) = StateWithExtensionsMut::<Token2022Account>::unpack(
        account.data_as_mut_slice(),
    ) else {
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
    NativeTokenNormalization::Normalized(rent_exempt_reserve)
}

fn normalize_token_2022_projected_token_account(
    account: &mut AccountSharedData,
) -> bool {
    let rent_exempt_reserve = {
        let Ok(mut state) = StateWithExtensionsMut::<Token2022Account>::unpack(
            account.data_as_mut_slice(),
        ) else {
            return false;
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
    if let Some(rent_exempt_reserve) = rent_exempt_reserve {
        account.set_lamports(rent_exempt_reserve);
    }
    true
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
    account: &AccountSharedData,
) -> Option<AtaInfo> {
    // The account must be owned by the SPL Token program (legacy) or Token-2022
    let token_program_owner = account.owner();
    let is_spl_token = *token_program_owner == spl_token::id();
    let is_token_2022 = *token_program_owner == TOKEN_2022_PROGRAM_ID;
    if !(is_spl_token || is_token_2022) {
        return None;
    }

    // Parse the token account data to extract mint and token owner
    // Layout (at least the first 64 bytes):
    // 0..32  -> mint Pubkey
    // 32..64 -> owner Pubkey (the wallet the ATA belongs to)
    let data = account.data();
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
    if !(is_spl_token || is_token_2022) || !account.delegated() {
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

pub fn try_get_rent_pending_ata_info(
    pubkey: &Pubkey,
    account: &AccountSharedData,
) -> Option<RentPendingAtaInfo> {
    if !account.delegated()
        || account.ephemeral()
        || account.confined()
        || account.undelegating()
    {
        return None;
    }

    let token_program = account.owner();
    if !is_supported_token_program(token_program) {
        return None;
    }
    try_get_rent_pending_ata_info_for_token_program(
        pubkey,
        account,
        token_program,
    )
}

/// Parses a rent-pending ATA after scheduling has marked it undelegating.
/// Callers must separately prove rent-pending materialization metadata was
/// recorded before trusting this shape.
pub fn try_get_undelegating_rent_pending_ata_info(
    pubkey: &Pubkey,
    account: &AccountSharedData,
) -> Option<RentPendingAtaInfo> {
    if account.delegated()
        || account.ephemeral()
        || account.confined()
        || !account.undelegating()
    {
        return None;
    }

    [TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID]
        .into_iter()
        .find_map(|token_program| {
            try_get_rent_pending_ata_info_for_token_program(
                pubkey,
                account,
                &token_program,
            )
        })
}

fn is_supported_token_program(token_program: &Pubkey) -> bool {
    *token_program == TOKEN_PROGRAM_ID
        || *token_program == TOKEN_2022_PROGRAM_ID
}

fn try_get_rent_pending_ata_info_for_token_program(
    pubkey: &Pubkey,
    account: &AccountSharedData,
    token_program: &Pubkey,
) -> Option<RentPendingAtaInfo> {
    let token_account =
        parse_token_account_for_rent_pending(token_program, account.data())?;
    if token_account.close_authority
        != COption::Some(RENT_PENDING_ATA_CLOSE_AUTHORITY)
        || token_account.is_native.is_some()
    {
        return None;
    }
    // Default owner/mint are reserved: they act as the legacy-decode sentinel
    // for persisted MagicContext materializations.
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

    let (eata_pubkey, _) = try_derive_eata_address_and_bump(
        &token_account.owner,
        &token_account.mint,
    )?;

    Some(RentPendingAtaInfo {
        ata_pubkey: *pubkey,
        eata_pubkey,
        token_program: *token_program,
        wallet_owner: token_account.owner,
        mint: token_account.mint,
        amount: token_account.amount,
        token_account_data_len: account.data().len() as u64,
    })
}

fn parse_token_account_for_rent_pending(
    token_program: &Pubkey,
    data: &[u8],
) -> Option<ParsedTokenAccount> {
    if *token_program == TOKEN_PROGRAM_ID {
        let account = SplAccount::unpack(data).ok()?;
        Some(ParsedTokenAccount {
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
        Some(ParsedTokenAccount {
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
        ata_account: &AccountSharedData,
    ) -> Option<AccountSharedData> {
        let token_program_owner = ata_account.owner();
        let is_spl_token = *token_program_owner == TOKEN_PROGRAM_ID;
        let is_token_2022 = *token_program_owner == TOKEN_2022_PROGRAM_ID;
        if !(is_spl_token || is_token_2022) {
            return None;
        }

        let data = ata_account.data();
        if data.len() < 72 {
            return None;
        }
        if &data[0..32] != self.mint.as_ref()
            || &data[32..64] != self.owner.as_ref()
        {
            return None;
        }

        let mut projected = ata_account.clone();
        projected.data_as_mut_slice()[64..72]
            .copy_from_slice(&self.amount.to_le_bytes());
        if !normalize_projected_token_account_for_local_clone(&mut projected) {
            return None;
        }
        Some(projected)
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
        let base_ata = AccountSharedData::from(Account {
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
            .project_into_ata_account(&base_ata)
            .expect("ATA should project");
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
        let base_ata = AccountSharedData::from(Account {
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
            .project_into_ata_account(&base_ata)
            .expect("native ATA should project");
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
        let base_ata = AccountSharedData::from(Account {
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
            .project_into_ata_account(&base_ata)
            .expect("Token-2022 ATA should project");
        assert_eq!(projected.owner(), &TOKEN_2022_PROGRAM_ID);
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
    fn rent_pending_ata_uses_rent_sysvar_close_authority() {
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
            close_authority: COption::Some(RENT_PENDING_ATA_CLOSE_AUTHORITY),
        };

        let mut data = vec![0u8; SplAccount::LEN];
        SplAccount::pack(token_account, &mut data).unwrap();
        let mut account = AccountSharedData::from(Account {
            owner: TOKEN_PROGRAM_ID,
            data,
            lamports: Rent::default().minimum_balance(SplAccount::LEN),
            executable: false,
            ..Default::default()
        });
        account.set_delegated(true);

        let info = try_get_rent_pending_ata_info(&ata, &account)
            .expect("rent-pending ATA should be detected");
        assert_eq!(info.ata_pubkey, ata);
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
            try_get_rent_pending_ata_info(&ata, &default_close_authority)
                .is_none()
        );

        let mut undelegating_account = account.clone();
        undelegating_account.set_undelegating(true);
        assert!(try_get_rent_pending_ata_info(&ata, &undelegating_account)
            .is_none());

        let mut scheduled_undelegating_account = account.clone();
        scheduled_undelegating_account.set_owner(Pubkey::new_unique());
        scheduled_undelegating_account.set_delegated(false);
        scheduled_undelegating_account.set_undelegating(true);
        let undelegating_info = try_get_undelegating_rent_pending_ata_info(
            &ata,
            &scheduled_undelegating_account,
        )
        .expect("scheduled undelegating rent-pending ATA should be detected");
        assert_eq!(undelegating_info.ata_pubkey, ata);
        assert_eq!(undelegating_info.wallet_owner, wallet_owner);
        assert_eq!(undelegating_info.mint, mint);
        assert_eq!(undelegating_info.amount, 9);

        let mut not_delegated = account.clone();
        not_delegated.set_delegated(false);
        assert!(try_get_rent_pending_ata_info(&ata, &not_delegated).is_none());

        let mut ephemeral_account = account.clone();
        ephemeral_account.set_ephemeral(true);
        assert!(
            try_get_rent_pending_ata_info(&ata, &ephemeral_account).is_none()
        );

        let mut confined_account = account.clone();
        confined_account.set_confined(true);
        assert!(
            try_get_rent_pending_ata_info(&ata, &confined_account).is_none()
        );

        let mut wrong_owner = account.clone();
        wrong_owner.set_owner(Pubkey::new_unique());
        assert!(try_get_rent_pending_ata_info(&ata, &wrong_owner).is_none());

        let mut native_account = account.clone();
        let mut token = SplAccount::unpack(native_account.data()).unwrap();
        token.is_native = COption::Some(0);
        SplAccount::pack(token, native_account.data_as_mut_slice()).unwrap();
        assert!(try_get_rent_pending_ata_info(&ata, &native_account).is_none());
    }

    #[test]
    fn rent_pending_ata_rejects_default_owner_and_mint() {
        // Default owner/mint are reserved as the MagicContext legacy-decode
        // sentinel and must never classify as rent-pending.
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
                close_authority: COption::Some(
                    RENT_PENDING_ATA_CLOSE_AUTHORITY,
                ),
            };
            let mut data = vec![0u8; SplAccount::LEN];
            SplAccount::pack(token_account, &mut data).unwrap();
            let mut account = AccountSharedData::from(Account {
                owner: TOKEN_PROGRAM_ID,
                data,
                lamports: Rent::default().minimum_balance(SplAccount::LEN),
                executable: false,
                ..Default::default()
            });
            account.set_delegated(true);

            assert!(try_get_rent_pending_ata_info(&ata, &account).is_none());
        }
    }
}
