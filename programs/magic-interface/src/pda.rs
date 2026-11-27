use crate::Pubkey;

/// Crank signer PDA seed. The signer is derived per task authority under the
/// Crank program, so it is identical to the address used before cranks moved
/// to hydra: `["crank-executor", authority]`.
#[deprecated(
    since = "2.0.0",
    note = "Programs should verify Hydra crank execution with the instructions sysvar instead \
            (`ephemeral_rollups_sdk::crank::hydra::require_hydra_crank`). The crank signer is \
            kept only for legacy tasks and will be removed."
)]
pub const CRANK_SEED: &[u8] = b"crank-executor";

/// Derives the crank signer PDA a scheduled task authorized by `authority`
/// may use as a signer in its instructions.
#[deprecated(
    since = "2.0.0",
    note = "Programs should verify Hydra crank execution with the instructions sysvar instead \
            (`ephemeral_rollups_sdk::crank::hydra::require_hydra_crank`). The crank signer is \
            kept only for legacy tasks and will be removed."
)]
#[allow(deprecated)]
pub fn crank_signer_pda(authority: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[CRANK_SEED, authority.as_ref()], &crate::CRANK_PROGRAM_ID).0
}

/// Callback signer PDA info
pub const CALLBACK_SEED: &[u8] = b"callback-executor";
const CALLBACK_SIGNER_PDA: ([u8; 32], u8) = const_crypto::ed25519::derive_program_address(
    &[CALLBACK_SEED],
    crate::CALLBACK_PROGRAM_ID.as_array(),
);
pub const CALLBACK_SIGNER: Pubkey = Pubkey::new_from_array(CALLBACK_SIGNER_PDA.0);
pub const CALLBACK_SIGNER_BUMP: u8 = CALLBACK_SIGNER_PDA.1;
