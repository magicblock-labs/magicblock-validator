use solana_account::{Account, AccountSharedData, ReadableAccount};
use solana_program::{program_option::COption, program_pack::Pack, rent::Rent};
use solana_pubkey::{pubkey, Pubkey};
use spl_token::state::{Account as SplAccount, AccountState};

// Shared program IDs and helper functions for SPL Token, Associated Token, and eATA programs.

// SPL Token Program ID (Tokenkeg...)
pub const SPL_TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

// Associated Token Account Program ID (ATokenG...)
pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

// Enhanced ATA (eATA) Program ID (5iC4wK...)
pub const EATA_PROGRAM_ID: Pubkey =
    pubkey!("5iC4wKZizyxrKh271Xzx3W4Vn2xUyYvSGHeoB2mdw5HA");

// Derive the standard ATA address for a given wallet owner and mint.
pub fn derive_ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), SPL_TOKEN_PROGRAM_ID.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
    .0
}

// Try to derive the ATA address returning both address and bump if derivation succeeds.
pub fn try_derive_ata_address_and_bump(
    owner: &Pubkey,
    mint: &Pubkey,
) -> Option<(Pubkey, u8)> {
    Pubkey::try_find_program_address(
        &[owner.as_ref(), SPL_TOKEN_PROGRAM_ID.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
}

// Derive the eATA PDA for a given wallet owner and mint.
pub fn derive_eata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), mint.as_ref()],
        &EATA_PROGRAM_ID,
    )
    .0
}

// Try to derive the eATA PDA returning both address and bump if derivation succeeds.
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

impl MaybeIntoAta<AccountSharedData> for AccountSharedData {
    fn maybe_into_ata(
        &self,
        owner_program: Pubkey,
    ) -> Option<AccountSharedData> {
        // Only proceed if the provided owner matches eATA program
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
