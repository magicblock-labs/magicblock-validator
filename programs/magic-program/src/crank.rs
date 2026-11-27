//! Helpers shared by the Crank program and the task scheduler for cranks that
//! sign with the task authority's crank signer PDA.
//!
//! Hydra runs scheduled instructions top-level and cannot produce PDA
//! signatures, so a task that uses its crank signer is stored in its hydra
//! crank as a single [`CrankInstruction::ExecuteCrank`] that wraps the task's
//! instructions. The Crank program then invokes them with the crank signer.

// The crank signer is deprecated for programs but still executed here.
#![allow(deprecated)]

use std::collections::HashMap;

use magic_program_api::{CRANK_PROGRAM_ID, instruction::CrankInstruction, pda::crank_signer_pda};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use solana_sdk_ids::sysvar::instructions as instructions_sysvar;

/// Index of the hydra crank in `ExecuteCrank`'s accounts.
pub const EXECUTE_CRANK_CRANK_IDX: u16 = 0;
/// Index of the crank signer PDA in `ExecuteCrank`'s accounts.
pub const EXECUTE_CRANK_SIGNER_IDX: u16 = 1;
/// Index of the instructions sysvar in `ExecuteCrank`'s accounts.
pub const EXECUTE_CRANK_SYSVAR_IDX: u16 = 2;

/// Hydra cranks use a 32-byte seed where Magic Program tasks use an `i64` ID.
/// The seed is `sha256(authority, task_id)`, so each authority gets its own
/// crank namespace and the crank of a task can be derived without a lookup.
pub fn task_crank_seed(authority: &Pubkey, task_id: i64) -> [u8; 32] {
    solana_sha256_hasher::hashv(&[authority.as_ref(), &task_id.to_le_bytes()]).to_bytes()
}

/// Whether any of `instructions` references the crank signer of `authority`.
pub fn uses_crank_signer(authority: &Pubkey, instructions: &[Instruction]) -> bool {
    let signer = crank_signer_pda(authority);
    instructions
        .iter()
        .any(|ix| ix.accounts.iter().any(|meta| meta.pubkey == signer))
}

/// Builds the `ExecuteCrank` instruction stored as the template of `crank`.
///
/// Hydra's `Trigger` compares the template byte-for-byte with the
/// instructions sysvar, which records each account with its
/// *transaction-level* flags. Accounts are therefore listed once, and an
/// account is writable if any embedded instruction writes it. The crank is
/// writable because `Trigger` writes it in the same transaction. No outer
/// account is a signer: hydra rejects templates with signer metas.
pub fn execute_crank_instruction(
    crank: Pubkey,
    authority: Pubkey,
    task_id: i64,
    instructions: Vec<Instruction>,
) -> Instruction {
    let mut accounts = vec![
        AccountMeta::new(crank, false),
        AccountMeta::new_readonly(crank_signer_pda(&authority), false),
        AccountMeta::new_readonly(instructions_sysvar::ID, false),
    ];
    let mut positions: HashMap<Pubkey, usize> =
        accounts.iter().enumerate().map(|(idx, meta)| (meta.pubkey, idx)).collect();
    let mut add = |pubkey: Pubkey, is_writable: bool| match positions.get(&pubkey) {
        Some(&idx) => accounts[idx].is_writable |= is_writable,
        None => {
            positions.insert(pubkey, accounts.len());
            accounts.push(AccountMeta {
                pubkey,
                is_signer: false,
                is_writable,
            });
        }
    };
    for instruction in &instructions {
        add(instruction.program_id, false);
        for meta in &instruction.accounts {
            add(meta.pubkey, meta.is_writable);
        }
    }
    Instruction::new_with_wincode(
        CRANK_PROGRAM_ID,
        &CrankInstruction::ExecuteCrank { authority, task_id, instructions },
        accounts,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crank_signer_pda_is_unchanged() {
        // Same derivation the Crank program used before cranks moved to hydra.
        let authority = Pubkey::new_unique();
        let (expected, _) = Pubkey::find_program_address(
            &[b"crank-executor", authority.as_ref()],
            &Pubkey::from_str_const("Crank11111111111111111111111111111111111111"),
        );
        assert_eq!(crank_signer_pda(&authority), expected);
    }

    #[test]
    fn template_lists_accounts_once_with_merged_writability() {
        let crank = Pubkey::new_unique();
        let authority = Pubkey::new_unique();
        let signer = crank_signer_pda(&authority);
        let shared = Pubkey::new_unique();
        let program = Pubkey::new_unique();
        let instructions = vec![
            Instruction::new_with_bytes(
                program,
                &[],
                vec![
                    AccountMeta::new_readonly(signer, true),
                    AccountMeta::new_readonly(shared, false),
                ],
            ),
            Instruction::new_with_bytes(program, &[], vec![AccountMeta::new(shared, false)]),
        ];
        let ix = execute_crank_instruction(crank, authority, 7, instructions);
        assert_eq!(
            ix.accounts,
            vec![
                AccountMeta::new(crank, false),
                AccountMeta::new_readonly(signer, false),
                AccountMeta::new_readonly(instructions_sysvar::ID, false),
                AccountMeta::new_readonly(program, false),
                AccountMeta::new(shared, false),
            ]
        );
    }

    #[test]
    fn crank_seed_is_namespaced_by_authority_and_id() {
        let a = Pubkey::new_unique();
        let b = Pubkey::new_unique();
        assert_eq!(task_crank_seed(&a, 1), task_crank_seed(&a, 1));
        assert_ne!(task_crank_seed(&a, 1), task_crank_seed(&a, 2));
        assert_ne!(task_crank_seed(&a, 1), task_crank_seed(&b, 1));
    }
}
