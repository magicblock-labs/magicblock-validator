use magicblock_core::token_programs::{
    EATA_PROGRAM_ID, EphemeralAta, derive_ata_with_token_program, try_derive_eata_address_and_bump,
};
use solana_account::{AccountBuilder, AccountMode, AccountSharedData, ReadableAccount};
use solana_program_option::COption;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use spl_token_2022::{extension::StateWithExtensions, state::Account as TokenAccount};

use crate::delegation;

/// Recognizes valid eATA data at its derived address, including while DLP-owned.
pub(crate) fn is_raw_eata(pubkey: Pubkey, account: &AccountBuilder) -> bool {
    let image = account.read();
    eata_data(pubkey, image.owner(), image.data()).is_some()
}

/// Restores eSPL ownership and makes a validated raw eATA readonly, never locally authoritative.
pub(crate) fn normalize_raw(account: AccountBuilder) -> AccountBuilder {
    account.owner(EATA_PROGRAM_ID).mode(AccountMode::ReadOnly)
}

/// The canonical eATA derived from a token account's owner and mint.
pub(crate) fn companion(pubkey: Pubkey, account: &AccountBuilder) -> Option<Pubkey> {
    let image = account.read();
    let token = token_account(pubkey, image.owner(), image.mode(), image.data())?;
    try_derive_eata_address_and_bump(&token.owner, &token.mint).map(|(pubkey, _)| pubkey)
}

/// Checks whether this delegated or transient ATA is a projection of the given eATA.
/// Projections have the default pubkey as close authority, preventing local closure.
pub(crate) fn is_projection_of(ata: Pubkey, eata: Pubkey, account: &AccountSharedData) -> bool {
    if !matches!(
        account.mode(),
        AccountMode::Delegated | AccountMode::Transient
    ) {
        return false;
    }
    let Some(token) = token_account(ata, *account.owner(), account.mode(), account.data()) else {
        return false;
    };
    token.close_authority == COption::Some(Pubkey::default())
        && try_derive_eata_address_and_bump(&token.owner, &token.mint)
            .is_some_and(|(pubkey, _)| pubkey == eata)
}

/// Returns legacy-token and Token-2022 ATA addresses for a validated eATA image, including DLP ownership.
/// Both are needed because eATA data does not identify the token program.
pub(crate) fn candidates(pubkey: Pubkey, owner: Pubkey, data: &[u8]) -> Option<[Pubkey; 2]> {
    let eata = eata_data(pubkey, owner, data)?;
    Some(
        [spl_token_2022::id(), spl_token::id()]
            .map(|program| derive_ata_with_token_program(&eata.owner, &eata.mint, &program)),
    )
}

/// Projects an eATA's balance and delegation slot onto a canonical ATA.
/// Preserves the base ATA's token program and extensions; native-token ATAs retain only rent lamports.
pub(crate) fn project(
    ata: Pubkey,
    base: AccountBuilder,
    eata: Pubkey,
    delegated: &AccountBuilder,
    record: &[u8],
    authority: Pubkey,
) -> Option<AccountBuilder> {
    let metadata = delegation::record(record)?;
    if metadata.owner != EATA_PROGRAM_ID || metadata.authority != authority {
        return None;
    }
    let image = delegated.read();
    let eata_state = eata_data(eata, image.owner(), image.data())?;
    let image = base.read();
    token_account(ata, image.owner(), image.mode(), image.data())?;
    let projected = eata_state.project_into_ata_account(base)?;
    Some(projected.slot(metadata.delegation_slot).mode(AccountMode::Delegated))
}

/// Recognizes an initialized token account at its canonical ATA address.
fn token_account(
    pubkey: Pubkey,
    program: Pubkey,
    mode: AccountMode,
    data: &[u8],
) -> Option<TokenAccount> {
    // Scheduling undelegation changes the owner to DLP but retains the token layout.
    let transient = program == dlp_api::id() && mode == AccountMode::Transient;
    let token = if program == spl_token::id() {
        TokenAccount::unpack(data).ok()?
    } else if program == spl_token_2022::id() || transient {
        StateWithExtensions::<TokenAccount>::unpack(data).ok()?.base
    } else {
        return None;
    };
    let canonical =
        |program| derive_ata_with_token_program(&token.owner, &token.mint, &program) == pubkey;
    let canonical = if transient {
        [spl_token::id(), spl_token_2022::id()].into_iter().any(canonical)
    } else {
        canonical(program)
    };
    canonical.then_some(token)
}

/// An eATA is trusted only when its owner and derived address agree with its data.
fn eata_data(pubkey: Pubkey, owner: Pubkey, data: &[u8]) -> Option<EphemeralAta> {
    if owner != EATA_PROGRAM_ID && owner != dlp_api::id() {
        return None;
    }
    let eata = EphemeralAta::try_from_account_data(data)?;
    let (address, bump) = try_derive_eata_address_and_bump(&eata.owner, &eata.mint)?;
    (address == pubkey && eata.bump == bump).then_some(eata)
}

#[cfg(test)]
mod test;
