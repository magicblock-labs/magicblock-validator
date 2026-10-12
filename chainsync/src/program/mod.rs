//! Converts remote loader layouts into Engine's ELF account format.

use std::mem::{offset_of, size_of};

use solana_account::{AccountBuilder, AccountMode};
use solana_loader_v3_interface::state::UpgradeableLoaderState;
use solana_loader_v4_interface::state::{LoaderV4State, LoaderV4Status};
use solana_pubkey::Pubkey;
use solana_rent::Rent;
use solana_sdk_ids::{bpf_loader, bpf_loader_deprecated, bpf_loader_upgradeable, loader_v4};

use crate::{Error, Result};

/// Accepts supported loader layouts; Loader V3 requires its ProgramData account.
/// The resulting ELF account is read-only and rent-funded for its normalized size.
pub(super) fn normalize(
    account: AccountBuilder,
    program_data: Option<AccountBuilder>,
    rent: &Rent,
) -> Result<AccountBuilder> {
    // Legacy ABI keeps Loader V1 ownership; newer ELF uses Loader V4 ownership.
    match account.read().owner() {
        id if id == bpf_loader_deprecated::ID => Ok(elf_account(account, id, rent)),
        id if id == bpf_loader::ID => Ok(elf_account(account, loader_v4::ID, rent)),
        id if id == bpf_loader_upgradeable::ID => {
            let program_data =
                program_data.ok_or(Error::Program("Loader V3 ProgramData is missing"))?;
            normalize_program_data(program_data, rent)
        }
        id if id == loader_v4::ID => {
            let elf = v4_elf(account.read().data())?.to_vec();
            Ok(elf_account(account.data(elf), id, rent))
        }
        _ => Err(Error::Program("unsupported loader")),
    }
}

/// Converts Loader V3 ProgramData into Engine's ELF representation.
pub(super) fn normalize_program_data(
    account: AccountBuilder,
    rent: &Rent,
) -> Result<AccountBuilder> {
    let elf = v3_elf(account.read().data())?.to_vec();
    Ok(elf_account(account.data(elf), loader_v4::ID, rent))
}

/// Configures a normalized ELF account for read-only Engine execution.
fn elf_account(account: AccountBuilder, owner: Pubkey, rent: &Rent) -> AccountBuilder {
    let len = account.read().data().len();
    account
        .lamports(rent.minimum_balance(len))
        .owner(owner)
        .mode(AccountMode::ReadOnly)
        .executable(true)
}

/// Returns the ELF payload from a valid Loader V3 ProgramData account.
fn v3_elf(data: &[u8]) -> Result<&[u8]> {
    let metadata = data
        .get(..UpgradeableLoaderState::size_of_programdata_metadata())
        .ok_or(Error::Program("Loader V3 ProgramData is too short"))?;
    let state = bincode::deserialize::<UpgradeableLoaderState>(metadata)?;
    if !matches!(state, UpgradeableLoaderState::ProgramData { .. }) {
        return Err(Error::Program("Loader V3 companion is not ProgramData"));
    }
    Ok(&data[metadata.len()..])
}

/// Returns deployed Loader V4 ELF and rejects invalid or retracted states.
fn v4_elf(data: &[u8]) -> Result<&[u8]> {
    let offset = LoaderV4State::program_data_offset();
    let header = data.get(..offset).ok_or(Error::Program("Loader V4 account is too short"))?;
    // Decode the discriminant as bytes so malformed input cannot create an invalid enum.
    let status_offset = offset_of!(LoaderV4State, status);
    let bytes = header
        .get(status_offset..status_offset + size_of::<u64>())
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(Error::Program("Loader V4 status is missing"))?;
    let status = u64::from_le_bytes(bytes);
    if status == LoaderV4Status::Retracted as u64 {
        return Err(Error::Program("Loader V4 program is retracted"));
    }
    if status != LoaderV4Status::Deployed as u64 && status != LoaderV4Status::Finalized as u64 {
        return Err(Error::Program("Loader V4 status is invalid"));
    }
    Ok(&data[offset..])
}

#[cfg(test)]
mod test;
