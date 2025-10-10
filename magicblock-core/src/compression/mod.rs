use light_compressed_account::address::derive_address;
use light_sdk::light_hasher::hash_to_field_size::hashv_to_bn254_field_size_be_const_array;
use solana_pubkey::Pubkey;

const ADDRESS_TREE: Pubkey =
    Pubkey::from_str_const("EzKE84aVTkCUhDHLELqyJaq1Y7UVVmqxXqZjVHwHY3rK");
const PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("DEL2rPzhFaS5qzo8XY9ZNxSzuunWueySq3p2dxJfwPbT");

/// Derives a CDA (Compressed derived Address) from a PDA (Program derived Address)
/// of a compressed account we want to use in our validator in uncompressed form.
pub fn derive_cda_from_pda(pda: &Pubkey) -> Pubkey {
    // Since the PDA is already unique we use the delagation program's id
    // as a program id.
    let seed =
        hashv_to_bn254_field_size_be_const_array::<3>(&[&pda.to_bytes()])
            .unwrap();
    let address =
        derive_address(&seed, &ADDRESS_TREE.to_bytes(), &PROGRAM_ID.to_bytes());
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
            pubkey!("1334SrUvQtX2JG1aqcDyrGBsLfbwnFTSiUgpcnsxfdFm")
        );
    }
}
