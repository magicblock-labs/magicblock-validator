use ed25519_dalek::{PublicKey, SecretKey};
use solana_clock::Slot;
use solana_keypair::Keypair;
use solana_signer::Signer;

/// This derives a keypair from the provided authority keypair, and given seeds which
/// here are slot and sub_slot.
/// Its goal is to be deterministic such that a keypair can be _found_ at a later date
/// given the same authority, cycling through slot/sub_slot combinations.
/// Using slot and sub_slot as seeds allows is only one option and we may change this
/// to use a different source for seeds in the future (as long as they are deterministic).
pub fn derive_keypair(
    authority: &Keypair,
    slot: Slot,
    sub_slot: Slot,
) -> Keypair {
    let mut seeds = authority.pubkey().to_bytes().to_vec();
    seeds.extend_from_slice(&slot.to_le_bytes());
    seeds.extend_from_slice(&sub_slot.to_le_bytes());
    derive_from_keypair(authority, &seeds)
}

fn derive_from_keypair(keypair: &Keypair, message: &[u8]) -> Keypair {
    let sig = keypair.sign_message(message);
    derive_insecure(sig.as_ref())
}

fn derive_insecure(message: &[u8]) -> Keypair {
    let hash = <sha3::Sha3_512 as sha3::Digest>::digest(message);
    let seed = &hash.as_slice()[0..32];

    // Create a keypair using the seed bytes
    let secret = SecretKey::from_bytes(seed).unwrap();
    let public = PublicKey::from(&secret);

    // Convert to Solana Keypair format
    let mut keypair_bytes = [0u8; 64];
    keypair_bytes[0..32].copy_from_slice(secret.as_bytes());
    keypair_bytes[32..64].copy_from_slice(&public.to_bytes());

    Keypair::from_bytes(&keypair_bytes).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_keypair_is_deterministic() {
        let authority = Keypair::new();
        let mut first = vec![];
        for slot in 0..100 {
            for sub_slot in 0..100 {
                let keypair = derive_keypair(&authority, slot, sub_slot);
                first.push(keypair.to_bytes());
            }
        }
        let mut second = vec![];
        for slot in 0..100 {
            for sub_slot in 0..100 {
                let keypair = derive_keypair(&authority, slot, sub_slot);
                second.push(keypair.to_bytes());
            }
        }

        assert_eq!(first, second);
    }
}
