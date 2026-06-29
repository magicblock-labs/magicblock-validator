//! Minimal, self-contained client builders for the hydra ephemeral crank
//! program.
//!
//! The validator's RPC client stack pins `solana-rpc-client-types 4.0.0`, which
//! caps `solana-address` below the `^2.6` that the upstream `hydra-api` crate
//! requires; the two cannot be unified in a single build. Rather than move the
//! whole validator onto pre-release RPC crates, we vendor the small, stable
//! slice of hydra's wire format that the scheduler needs. The integration is
//! pinned to a specific hydra revision, so this layout will not drift
//! unexpectedly.
//!
//! Mirrors `hydra-api` rev `1fc9086` (`crates/hydra-api/src/instruction.rs`,
//! the `ephemeral` builders). See that file for the authoritative wire format.

use magicblock_program::EPHEMERAL_VAULT_PUBKEY;
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

/// Ephemeral-rollup hydra program id (`eHyd5BU8QffvHi4GnXwxrK4WpS7pM2x9UGKHBWii7mf`).
pub const EPHEMERAL_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
    9, 141, 175, 94, 62, 38, 45, 106, 31, 231, 193, 37, 229, 238, 178, 89, 200,
    202, 82, 70, 56, 177, 52, 125, 239, 164, 240, 139, 173, 185, 238, 142,
]);

/// Seed prefix for the crank PDA: `[b"crank", seed]`.
const CRANK_SEED_PREFIX: &[u8] = b"crank";

/// Instruction discriminators (one byte each).
const IX_CREATE: u8 = 0;
const IX_CANCEL: u8 = 2;

/// Per-meta writable flag in the scheduled-ix wire format.
const META_FLAG_WRITABLE: u8 = 0b0000_0010;

/// Flat per-trigger reward (lamports) paid to the cranker. Matches
/// `hydra_api::consts::CRANKER_REWARD` for the deployed program, which is built
/// without the `hydra-api/ephemeral` feature, so `BASE_FEE_LAMPORTS = 5_000`
/// and `CRANKER_REWARD = 2 * BASE_FEE_LAMPORTS`.
pub const CRANKER_REWARD: u64 = 10_000;

/// One scheduled-ix account meta as stored on-chain. Scheduled instructions may
/// not carry signer flags, so only the writable bit is represented.
pub struct SchedMeta {
    pub pubkey: [u8; 32],
    pub is_writable: bool,
}

/// One scheduled instruction template.
pub struct ScheduledIx<'a> {
    pub program_id: [u8; 32],
    pub metas: &'a [SchedMeta],
    pub data: &'a [u8],
}

/// Scheduling knobs for a hydra `Create` instruction.
pub struct CreateArgs<'a> {
    pub seed: [u8; 32],
    /// All-zeros = unkillable (no cancel authority).
    pub authority: [u8; 32],
    pub start_slot: u64,
    pub interval_slots: u64,
    /// `0` on the wire means "infinite"; the scheduler always sets a finite
    /// iteration count.
    pub remaining: u64,
    pub priority_tip: u64,
    pub cu_limit: u32,
    /// Scheduled instructions in execution order. Must be non-empty.
    pub scheduled: &'a [ScheduledIx<'a>],
}

impl CreateArgs<'_> {
    /// Serializes the `Create` instruction data exactly as hydra's on-chain
    /// entrypoint parses it.
    fn serialize(&self) -> Vec<u8> {
        // 1 discriminator + 100-byte fixed prefix + variable body.
        let body_len: usize = self
            .scheduled
            .iter()
            .map(|s| 1 + 2 + 32 + 33 * s.metas.len() + s.data.len())
            .sum();
        let mut data = Vec::with_capacity(1 + 100 + body_len);

        data.push(IX_CREATE);
        data.extend_from_slice(&self.seed);
        data.extend_from_slice(&self.authority);
        data.extend_from_slice(&self.start_slot.to_le_bytes());
        data.extend_from_slice(&self.interval_slots.to_le_bytes());
        data.extend_from_slice(&self.remaining.to_le_bytes());
        data.extend_from_slice(&self.priority_tip.to_le_bytes());
        data.extend_from_slice(&self.cu_limit.to_le_bytes());

        for s in self.scheduled {
            data.push(s.metas.len() as u8);
            data.extend_from_slice(&(s.data.len() as u16).to_le_bytes());
            data.extend_from_slice(&s.program_id);
            for m in s.metas {
                let flag = if m.is_writable { META_FLAG_WRITABLE } else { 0 };
                data.push(flag);
                data.extend_from_slice(&m.pubkey);
            }
            data.extend_from_slice(s.data);
        }

        data
    }
}

/// Builders targeting the ephemeral-rollup hydra program.
pub mod ephemeral {
    use super::*;

    /// Derives `(crank_pda, bump)` under the ephemeral hydra program.
    pub fn find_crank_pda(seed: &[u8; 32]) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[CRANK_SEED_PREFIX, seed],
            &EPHEMERAL_PROGRAM_ID,
        )
    }

    /// Builds a hydra `Create` instruction for the ephemeral program.
    pub fn create(
        sponsor: Pubkey,
        crank: Pubkey,
        args: &CreateArgs<'_>,
    ) -> Instruction {
        Instruction {
            program_id: EPHEMERAL_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(sponsor, true),
                AccountMeta::new(crank, false),
                AccountMeta::new(EPHEMERAL_VAULT_PUBKEY, false),
                AccountMeta::new_readonly(magicblock_program::id(), false),
            ],
            data: args.serialize(),
        }
    }

    /// Builds a hydra `Cancel` instruction for the ephemeral program. The
    /// remaining crank balance is refunded to the ephemeral vault.
    pub fn cancel(authority: Pubkey, crank: Pubkey) -> Instruction {
        Instruction {
            program_id: EPHEMERAL_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new(authority, true),
                AccountMeta::new(crank, false),
                AccountMeta::new(EPHEMERAL_VAULT_PUBKEY, false),
                AccountMeta::new_readonly(magicblock_program::id(), false),
            ],
            data: vec![IX_CANCEL],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ephemeral_program_id_matches_base58() {
        assert_eq!(
            EPHEMERAL_PROGRAM_ID.to_string(),
            "eHyd5BU8QffvHi4GnXwxrK4WpS7pM2x9UGKHBWii7mf"
        );
    }

    #[test]
    fn create_data_layout_matches_wire_format() {
        let program_id = [7u8; 32];
        let acct = [9u8; 32];
        let metas = [SchedMeta {
            pubkey: acct,
            is_writable: true,
        }];
        let scheduled = [ScheduledIx {
            program_id,
            metas: &metas,
            data: &[1, 2, 3],
        }];
        let args = CreateArgs {
            seed: [1u8; 32],
            authority: [2u8; 32],
            start_slot: 5,
            interval_slots: 7,
            remaining: 9,
            priority_tip: 11,
            cu_limit: 0,
            scheduled: &scheduled,
        };
        let data = args.serialize();

        // 1 disc + 100 fixed prefix + (1 + 2 + 32 + 33*1 + 3) body.
        assert_eq!(data.len(), 1 + 100 + (1 + 2 + 32 + 33 + 3));
        assert_eq!(data[0], IX_CREATE);
        // seed starts right after the discriminator.
        assert_eq!(&data[1..33], &[1u8; 32]);
        // authority follows the seed.
        assert_eq!(&data[33..65], &[2u8; 32]);
        // The writable meta flag is encoded for the single scheduled account.
        let meta_flag_off = 1 + 100 + 1 + 2 + 32;
        assert_eq!(data[meta_flag_off], META_FLAG_WRITABLE);
    }
}
