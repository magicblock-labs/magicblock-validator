use paste::paste;

const CHUNKS_SEED: &[u8] = b"comittor_chunks";
const BUFFER_SEED: &[u8] = b"comittor_buffer";

macro_rules! seeds {
    ($prefix:ident, $bytes_const:expr) => {
        paste! {
            #[allow(clippy::needless_lifetimes)]
            pub fn [<$prefix _seeds>]<'a>(
                validator_auth: &'a ::solana_pubkey::Pubkey,
                pubkey: &'a ::solana_pubkey::Pubkey,
                blockhash: &'a ::solana_program::hash::Hash) -> [&'a [u8]; 5] {
                [
                    crate::ID.as_ref(),
                    $bytes_const,
                    validator_auth.as_ref(),
                    pubkey.as_ref(),
                    blockhash.as_ref(),
                ]
            }
            #[allow(clippy::needless_lifetimes)]
            pub fn [<$prefix _seeds_with_bump>]<'a>(
                validator_auth: &'a ::solana_pubkey::Pubkey,
                pubkey: &'a ::solana_pubkey::Pubkey,
                blockhash: &'a ::solana_program::hash::Hash,
                bump: &'a [u8],
            ) -> [&'a [u8]; 6] {
                [
                    crate::ID.as_ref(),
                    $bytes_const,
                    validator_auth.as_ref(),
                    pubkey.as_ref(),
                    blockhash.as_ref(),
                    bump,
                ]
            }
        }
    };
}

macro_rules! pda {
    ($prefix:ident) => {
        paste! {
            #[allow(clippy::needless_lifetimes)]
            pub fn [<$prefix _pda>]<'a>(
                validator_auth: &'a ::solana_pubkey::Pubkey,
                pubkey: &'a ::solana_pubkey::Pubkey,
                blockhash: &'a ::solana_program::hash::Hash,
            ) -> (::solana_pubkey::Pubkey, u8) {
                let program_id = &crate::id();
                let seeds = [<$prefix _seeds>](validator_auth, pubkey, blockhash);
                ::solana_pubkey::Pubkey::find_program_address(&seeds, program_id)
            }
            #[allow(clippy::needless_lifetimes)]
            pub fn [<try_ $prefix _pda_with_bump>]<'a>(
                validator_auth: &'a ::solana_pubkey::Pubkey,
                pubkey: &'a ::solana_pubkey::Pubkey,
                blockhash: &'a ::solana_program::hash::Hash,
                bump: &'a [u8],
            ) -> $crate::error::CommittorResult<::solana_pubkey::Pubkey> {
                let program_id = &crate::id();
                let seeds = [<$prefix _seeds_with_bump>](validator_auth, pubkey, blockhash, bump);
                Ok(::solana_pubkey::Pubkey::create_program_address(&seeds, program_id)?)
            }
        }
    };
}

seeds!(chunks, CHUNKS_SEED);
pda!(chunks);
seeds!(buffer, BUFFER_SEED);
pda!(buffer);

#[macro_export]
macro_rules! verified_seeds_and_pda {
    ($prefix:ident,
     $authority_info:ident,
     $pubkey:ident,
     $account_info:ident,
     $blockhash:ident,
     $bump:ident) => {{
        ::paste::paste! {
            let seeds = $crate::pdas::[<$prefix _seeds_with_bump>](
                $authority_info.key,
                $pubkey,
                &$blockhash,
                $bump,
            );
            let pda = $crate::pdas::[<try_ $prefix _pda_with_bump>](
                $authority_info.key,
                $pubkey,
                &$blockhash,
                $bump,
            )
            .inspect_err(|err| ::solana_program::msg!("ERR: {}", err))?;
            $crate::utils::assert_keys_equal($account_info.key, &pda, || {
                format!(
                    "Provided {} PDA does not match derived key '{}'",
                    stringify!($prefix),
                    pda
                )
            })?;
            (seeds, pda)
        }
    }};
}
