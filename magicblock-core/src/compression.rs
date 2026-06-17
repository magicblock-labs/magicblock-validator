use light_sdk::address::v2::derive_address;
use solana_pubkey::Pubkey;

// Light protocol V2 accounts:
// https://www.zkcompression.com/resources/addresses-and-urls#v2-2
pub const ADDRESS_TREE: Pubkey =
    Pubkey::from_str_const("amt2kaJA14v3urZbZvnc5v2np8jqvc4Z8zDep5wbtzx");
pub const OUTPUT_QUEUE: Pubkey =
    Pubkey::from_str_const("oq1na8gojfdUhsfCpyjNt6h4JaDWtHf1yQj4koBWfto");

/// Derives a CDA (Compressed derived Address) from a PDA (Program derived Address)
/// of a compressed account we want to use in our validator in uncompressed form.
pub fn derive_cda_from_pda(pda: &Pubkey) -> Pubkey {
    // Since the PDA is already unique we use the delegation program's id
    // as a program id.
    let (address, _seed) = derive_address(
        &[pda.as_array()],
        &ADDRESS_TREE.to_bytes().into(),
        &compressed_delegation_client::ID.to_bytes().into(),
    );
    Pubkey::new_from_array(address)
}

#[cfg(test)]
mod tests {
    use solana_pubkey::pubkey;

    use super::*;

    #[test]
    fn test_derive_cda_from_pda() {
        let pda = pubkey!("6pyGAQnqveUcHJ4iT1B6N72iJSBWcb6KRht315Fo7mLX");
        let cda = derive_cda_from_pda(&pda);
        assert_eq!(
            cda,
            pubkey!("13CJjg6sMzZ8Lsn1oyQggzcyq5nFHYt97i7bhMu7BNu9")
        );
    }
}
