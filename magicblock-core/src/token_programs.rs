use solana_account::{Account, AccountSharedData, ReadableAccount};
use solana_program::{program_option::COption, program_pack::Pack, rent::Rent};
use solana_pubkey::{pubkey, Pubkey};
use spl_token::state::{Account as SplAccount, AccountState};

// Shared program IDs and helper functions for SPL Token, Associated Token, and eATA programs.

// Token Program ID (Tokenkeg...)
pub const TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

// Associated Token Account Program ID (ATokenG...)
pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

// Enhanced ATA (eATA) Program ID (SPLxh1...)
pub const EATA_PROGRAM_ID: Pubkey =
    pubkey!("SPLxh1LVZzEkX99H6rqYizhytLWPZVV296zyYDPagv2");

/// Derives the standard Associated Token Account (ATA) address for the given wallet owner and token mint.
///
/// # Arguments
/// * `owner` - The public key of the account owner
/// * `mint` - The public key of the token mint
///
/// # Returns
/// The derived ATA address as `Pubkey`.
pub fn derive_ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), TOKEN_PROGRAM_ID.as_ref(), mint.as_ref()],
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
    Pubkey::try_find_program_address(
        &[owner.as_ref(), TOKEN_PROGRAM_ID.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
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
    let is_token_2022 = *token_program_owner == spl_token_2022::id();
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
    if account.owner() != &TOKEN_PROGRAM_ID || !account.delegated() {
        return None;
    }

    let data = account.data();
    if data.len() < 72 {
        return None;
    }

    let mint = Pubkey::new_from_array(data[0..32].try_into().ok()?);
    let owner = Pubkey::new_from_array(data[32..64].try_into().ok()?);
    let amount = u64::from_le_bytes(data[64..72].try_into().ok()?);

    let ata = derive_ata(&owner, &mint);
    if ata != *pubkey {
        return None;
    }

    let eata = EphemeralAta {
        owner,
        mint,
        amount,
    };

    Some((derive_eata(&owner, &mint), eata))
}

// ---------------- eATA -> ATA projection helpers ----------------

/// Try to convert an eATA account data buffer into a concrete SPL Token
/// ATA account (AccountSharedData) when the owning program matches the
/// expected eATA program. This is an extension trait implemented for
/// AccountSharedData.
pub trait MaybeIntoAta<T> {
    /// Attempts to convert `self` to an ATA form if `owner_program` equals
    /// the eATA program id. Returns None if not an eATA or parsing fails.
    fn maybe_into_ata(&self, owner_program: Pubkey) -> Option<T>;
}

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
}

impl From<EphemeralAta> for AccountSharedData {
    fn from(val: EphemeralAta) -> Self {
        let token_account = SplAccount {
            mint: val.mint,
            owner: val.owner,
            amount: val.amount,
            delegate: COption::None,
            state: AccountState::Initialized,
            is_native: COption::None,
            delegated_amount: 0,
            close_authority: COption::None,
        };

        let mut data = vec![0u8; SplAccount::LEN];
        SplAccount::pack(token_account, &mut data)
            .expect("pack spl token account");
        let lamports = Rent::default().minimum_balance(data.len());

        let account = Account {
            owner: spl_token::id(),
            data,
            lamports,
            executable: false,
            ..Default::default()
        };

        AccountSharedData::from(account)
    }
}

impl From<EphemeralAta> for Account {
    fn from(val: EphemeralAta) -> Self {
        // Encode as: owner(32) | mint(32) | amount(8)
        let mut data = Vec::with_capacity(72);
        data.extend_from_slice(val.owner.as_ref());
        data.extend_from_slice(val.mint.as_ref());
        data.extend_from_slice(&val.amount.to_le_bytes());

        Account {
            lamports: Rent::default().minimum_balance(data.len()),
            data,
            owner: EATA_PROGRAM_ID,
            executable: false,
            ..Default::default()
        }
    }
}

impl MaybeIntoAta<AccountSharedData> for AccountSharedData {
    fn maybe_into_ata(
        &self,
        owner_program: Pubkey,
    ) -> Option<AccountSharedData> {
        if owner_program != EATA_PROGRAM_ID {
            return None;
        }

        let data = self.data();
        // Expect at least owner(32) + mint(32) + amount(8)
        if data.len() < 72 {
            return None;
        }
        let owner = Pubkey::new_from_array(data[0..32].try_into().ok()?);
        let mint = Pubkey::new_from_array(data[32..64].try_into().ok()?);
        let amount = u64::from_le_bytes(data[64..72].try_into().ok()?);
        let eata = EphemeralAta {
            owner,
            mint,
            amount,
        };
        Some(eata.into())
    }
}
