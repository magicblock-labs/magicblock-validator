use super::*;

const SLOT: u64 = 12;
const ELF: &[u8] = &[1, 2, 3];

/// Encodes only the loader header so both valid and invalid layouts use the same status offset.
fn v4_header(status: u64) -> Vec<u8> {
    let mut data = vec![0; LoaderV4State::program_data_offset()];
    let offset = offset_of!(LoaderV4State, status);
    let status = status.to_le_bytes();
    data[offset..offset + status.len()].copy_from_slice(&status);
    data
}

/// Supported loader images normalize to rent-funded, read-only ELF without changing source slot.
#[test]
fn supported_loaders_normalize_to_rent_funded_readonly_elf() {
    let rent = Rent::default();
    for (source, owner) in [
        (bpf_loader_deprecated::ID, bpf_loader_deprecated::ID),
        (bpf_loader::ID, loader_v4::ID),
        (bpf_loader_upgradeable::ID, loader_v4::ID),
        (loader_v4::ID, loader_v4::ID),
    ] {
        let mut image = AccountBuilder::default().owner(source).slot(SLOT).data(ELF.to_vec());
        let companion = if source == bpf_loader_upgradeable::ID {
            let mut data = bincode::serialize(&UpgradeableLoaderState::ProgramData {
                slot: SLOT,
                upgrade_authority_address: None,
            })
            .unwrap();
            data.resize(UpgradeableLoaderState::size_of_programdata_metadata(), 0);
            data.extend_from_slice(ELF);
            Some(AccountBuilder::default().data(data).slot(SLOT))
        } else {
            None
        };
        if source == loader_v4::ID {
            let mut data = v4_header(LoaderV4Status::Deployed as u64);
            data.extend_from_slice(ELF);
            image = image.data(data);
        }
        let result = normalize(image, companion, &rent).unwrap();
        assert_eq!(result.read().data(), ELF);
        assert_eq!(result.read().owner(), owner);
        assert_eq!(result.read().mode(), AccountMode::ReadOnly);
        assert_eq!(result.read().slot(), SLOT);
        assert_eq!(result.read().lamports(), rent.minimum_balance(ELF.len()));
    }
}

/// Malformed, missing, wrong-kind, retracted, and unknown-status loader images are rejected.
#[test]
fn loader_normalization_rejects_missing_malformed_and_retracted_images() {
    assert!(v3_elf(&[]).is_err());
    let mut wrong = bincode::serialize(&UpgradeableLoaderState::Uninitialized).unwrap();
    wrong.resize(UpgradeableLoaderState::size_of_programdata_metadata(), 0);
    assert!(v3_elf(&wrong).is_err());
    let image = AccountBuilder::default().owner(bpf_loader_upgradeable::ID);
    assert!(normalize(image, None, &Rent::default()).is_err());
    assert!(v4_elf(&[]).is_err());
    for status in [LoaderV4Status::Retracted as u64, u64::MAX] {
        let data = v4_header(status);
        assert!(v4_elf(&data).is_err());
    }
}
