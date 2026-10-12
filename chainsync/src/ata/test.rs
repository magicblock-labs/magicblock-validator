use dlp_api::state::DelegationRecord;

use super::*;
use crate::transport::{
    Ata, DELEGATED_TOKEN_BALANCE, NATIVE_RESERVE, PROJECTION_SLOT, TOKEN_LAMPORTS,
};

/// Wire offset after the eATA's owner, mint, and amount fields.
const BUMP_OFFSET: usize = 72;

/// Both token programs project canonical identities, preserve extensions, and strip native spending authority.
#[test]
fn canonical_projection_preserves_extensions_and_native_reserve_for_both_token_programs() {
    for program in [spl_token::id(), spl_token_2022::id()] {
        for native in [false, true] {
            let authority = Pubkey::new_unique();
            let Ata {
                ata,
                base,
                eata,
                delegated,
                record,
            } = Ata::new(program, native, authority, PROJECTION_SLOT);
            assert_eq!(companion(ata, &base), Some(eata));
            assert!(
                candidates(eata, delegated.read().owner(), delegated.read().data())
                    .unwrap()
                    .contains(&ata)
            );
            let extensions = base.read().data()[TokenAccount::LEN..].to_vec();
            let result = project(ata, base, eata, &delegated, &record, authority).unwrap();
            let image = result.read();
            let token = token_account(ata, image.owner(), image.mode(), image.data()).unwrap();
            assert_eq!(token.amount, DELEGATED_TOKEN_BALANCE);
            assert_eq!(token.close_authority, COption::Some(Pubkey::default()));
            assert_eq!(token.is_native, COption::None);
            assert_eq!(image.owner(), program);
            assert_eq!(image.slot(), PROJECTION_SLOT);
            // Native spending moves to the delegation; only the token's explicit
            // rent reserve remains in the projection, not its original lamports.
            assert_eq!(
                image.lamports(),
                if native { NATIVE_RESERVE } else { TOKEN_LAMPORTS }
            );
            assert_eq!(&result.read().data()[TokenAccount::LEN..], extensions);
            assert!(is_projection_of(ata, eata, &result));
            let transient = result.mode(AccountMode::Transient);
            assert!(is_projection_of(ata, eata, &transient));
        }
    }
}

/// Noncanonical accounts, wrong authorities, malformed eATA identities, and confinement cannot project.
#[test]
fn projection_rejects_noncanonical_identities_wrong_authority_and_confinement() {
    let authority = Pubkey::new_unique();
    let Ata {
        ata,
        base,
        eata,
        delegated,
        record,
    } = Ata::new(spl_token::id(), false, authority, PROJECTION_SLOT);
    assert!(companion(Pubkey::new_unique(), &base).is_none());
    for (key, companion, authority) in [
        (Pubkey::new_unique(), eata, authority),
        (ata, eata, Pubkey::new_unique()),
        (ata, Pubkey::new_unique(), authority),
    ] {
        let projection = project(key, base.clone(), companion, &delegated, &record, authority);
        assert!(projection.is_none());
    }
    let mut invalid = delegated.read().data().to_vec();
    invalid[BUMP_OFFSET] = invalid[BUMP_OFFSET].wrapping_add(1);
    assert!(!is_raw_eata(eata, &delegated.clone().data(invalid)));
    let mut metadata = *delegation::record(&record).unwrap();
    metadata.authority = Pubkey::default();
    let mut confined_record = vec![0; DelegationRecord::size_with_discriminator()];
    metadata.to_bytes_with_discriminator(&mut confined_record).unwrap();
    let confined = delegation::account(delegated, &metadata);
    assert!(project(ata, base, eata, &confined, &confined_record, authority).is_none());
}
