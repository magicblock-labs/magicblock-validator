use light_sdk::address::v1::derive_address;
use solana_pubkey::Pubkey;

const ADDRESS_TREE: Pubkey =
    Pubkey::new_from_array(light_sdk::constants::ADDRESS_TREE_V1);

/// Derives a CDA (Compressed derived Address) from a PDA (Program derived Address)
/// of a compressed account we want to use in our validator in uncompressed form.
pub fn derive_cda_from_pda(pda: &Pubkey) -> Pubkey {
    // Since the PDA is already unique we use the delagation program's id
    // as a program id.
    // In v2 of light it will be verified to match the CPI initiator program.
    let program_id = dlp::id();
    let (address, _) =
        derive_address(&[pda.as_ref()], &ADDRESS_TREE, &program_id);
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
        assert_eq!(cda, pubkey!("12Bo41dhMABvdoidXXaAdT1arxvZZ7AsYQkY4ShGmZR"));
    }
}
