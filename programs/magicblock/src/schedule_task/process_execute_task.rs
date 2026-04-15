use std::collections::HashSet;

use magicblock_magic_program_api::pda::CRANK_SIGNER;
use solana_instruction::{error::InstructionError, Instruction};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;

use crate::{
    schedule_task::validate_cranks_instructions,
    utils::accounts::get_instruction_pubkey_with_idx,
    validator::validator_authority_id,
};

pub(crate) fn process_execute_crank(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    instructions: Vec<Instruction>,
) -> Result<(), InstructionError> {
    const VALIDATOR_IDX: u16 = 0;
    const CRANK_SIGNER_IDX: u16 = 1;

    let transaction_context = &invoke_context.transaction_context.clone();
    let ix_ctx = transaction_context.get_current_instruction_context()?;
    let ix_accs_len = ix_ctx.get_number_of_instruction_accounts() as usize;
    const ACCOUNTS_START: usize = CRANK_SIGNER_IDX as usize + 1;

    // Assert MagicBlock program
    ix_ctx
        .find_index_of_program_account(transaction_context, &crate::id())
        .ok_or_else(|| {
            ic_msg!(
                invoke_context,
                "ExecuteCrank ERR: Magic program account not found"
            );
            InstructionError::UnsupportedProgramId
        })?;

    // Assert enough accounts
    if ix_accs_len < ACCOUNTS_START {
        ic_msg!(
            invoke_context,
            "ExecuteCrank ERR: not enough accounts to execute crank ({}), need crank signer and instructions",
            ix_accs_len
        );
        return Err(InstructionError::NotEnoughAccountKeys);
    }

    // Assert Validator is signer
    // Only the validator can execute a crank
    let validator_pubkey =
        get_instruction_pubkey_with_idx(transaction_context, VALIDATOR_IDX)?;
    if validator_pubkey != &validator_authority_id() {
        ic_msg!(
            invoke_context,
            "ExecuteCrank ERR: validator pubkey {} is not the expected validator",
            validator_pubkey
        );
        return Err(InstructionError::IncorrectAuthority);
    }
    if !signers.contains(validator_pubkey) {
        ic_msg!(
            invoke_context,
            "ExecuteCrank ERR: validator pubkey {} is not in signers",
            validator_pubkey
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    // Assert Crank signer is provided
    let crank_signer_pubkey =
        get_instruction_pubkey_with_idx(transaction_context, CRANK_SIGNER_IDX)?;
    if crank_signer_pubkey != &CRANK_SIGNER {
        ic_msg!(
            invoke_context,
            "ExecuteCrank ERR: crank signer pubkey {} is not the expected Crank signer",
            crank_signer_pubkey
        );
        return Err(InstructionError::InvalidSeeds);
    }

    // Already validated when scheduling the task.
    // This check prevents the validator from manually sending transactions disguised as cranks.
    validate_cranks_instructions(invoke_context, &instructions)?;

    let len = instructions.len();
    for ix in instructions {
        invoke_context.native_invoke(ix.into(), &[CRANK_SIGNER])?;
    }

    ic_msg!(invoke_context, "Executed crank with {} instructions", len);

    Ok(())
}

#[cfg(test)]
mod test {
    use magicblock_magic_program_api::args::ScheduleTaskArgs;
    use solana_account::AccountSharedData;
    use solana_instruction::AccountMeta;
    use solana_keypair::Keypair;
    use solana_sdk_ids::system_program;

    use super::*;
    use crate::{
        test_utils::process_instruction,
        utils::instruction_utils::InstructionUtils,
        validator::init_validator_authority_if_needed,
    };

    pub fn complex_ix(payer: Pubkey) -> Instruction {
        let mut ix = InstructionUtils::noop_instruction(0);
        ix.accounts.push(AccountMeta::new(payer, false));
        ix
    }

    #[test]
    fn test_execute_task_simple() {
        init_validator_authority_if_needed(Keypair::new());
        let ix = InstructionUtils::execute_task_instruction(vec![
            InstructionUtils::noop_instruction(0),
        ]);
        let transaction_accounts = vec![
            (
                validator_authority_id(),
                AccountSharedData::new(0, 0, &system_program::id()),
            ),
            (
                CRANK_SIGNER,
                AccountSharedData::new(0, 0, &system_program::id()),
            ),
        ];
        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );
    }

    #[test]
    fn test_execute_task_complex() {
        init_validator_authority_if_needed(Keypair::new());
        let payer = Pubkey::new_unique();
        let ix =
            InstructionUtils::execute_task_instruction(vec![complex_ix(payer)]);
        let transaction_accounts = vec![
            (
                validator_authority_id(),
                AccountSharedData::new(0, 0, &system_program::id()),
            ),
            (
                CRANK_SIGNER,
                AccountSharedData::new(0, 0, &system_program::id()),
            ),
            (
                payer,
                AccountSharedData::new(1000000, 0, &system_program::id()),
            ),
        ];
        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );
    }

    #[test]
    fn fail_execute_task_without_crank_signer() {
        init_validator_authority_if_needed(Keypair::new());
        let ix = InstructionUtils::execute_task_instruction(vec![
            InstructionUtils::noop_instruction(0),
        ]);
        let transaction_accounts = vec![
            (
                validator_authority_id(),
                AccountSharedData::new(0, 0, &system_program::id()),
            ),
            (
                CRANK_SIGNER,
                AccountSharedData::new(0, 0, &system_program::id()),
            ),
        ];
        process_instruction(
            &ix.data,
            transaction_accounts,
            vec![],
            Err(InstructionError::NotEnoughAccountKeys),
        );
    }

    #[test]
    fn fail_execute_task_wrong_validator() {
        init_validator_authority_if_needed(Keypair::new());
        let ix = InstructionUtils::execute_task_instruction(vec![
            InstructionUtils::noop_instruction(0),
        ]);
        let wrong_validator = Pubkey::new_unique();
        let transaction_accounts = vec![
            (
                wrong_validator,
                AccountSharedData::new(0, 0, &system_program::id()),
            ),
            (
                CRANK_SIGNER,
                AccountSharedData::new(0, 0, &system_program::id()),
            ),
        ];
        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::IncorrectAuthority),
        );
    }

    #[test]
    fn fail_execute_task_validator_not_in_signers() {
        init_validator_authority_if_needed(Keypair::new());
        let mut ix = InstructionUtils::execute_task_instruction(vec![
            InstructionUtils::noop_instruction(0),
        ]);
        ix.accounts[0].is_signer = false;
        let transaction_accounts = vec![
            (
                validator_authority_id(),
                AccountSharedData::new(0, 0, &system_program::id()),
            ),
            (
                CRANK_SIGNER,
                AccountSharedData::new(0, 0, &system_program::id()),
            ),
        ];
        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::MissingRequiredSignature),
        );
    }

    #[test]
    fn fail_execute_task_wrong_crank_signer() {
        init_validator_authority_if_needed(Keypair::new());
        let ix = InstructionUtils::execute_task_instruction(vec![
            InstructionUtils::noop_instruction(0),
        ]);
        let wrong_crank_signer = Pubkey::new_unique();
        let transaction_accounts = vec![
            (
                validator_authority_id(),
                AccountSharedData::new(0, 0, &system_program::id()),
            ),
            (
                wrong_crank_signer,
                AccountSharedData::new(0, 0, &system_program::id()),
            ),
        ];
        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::InvalidSeeds),
        );
    }

    #[test]
    fn fail_execute_task_missing_accounts() {
        init_validator_authority_if_needed(Keypair::new());
        let payer = Pubkey::new_unique();
        let ix =
            InstructionUtils::execute_task_instruction(vec![complex_ix(payer)]);
        let transaction_accounts = vec![
            (
                validator_authority_id(),
                AccountSharedData::new(0, 0, &system_program::id()),
            ),
            (
                CRANK_SIGNER,
                AccountSharedData::new(0, 0, &system_program::id()),
            ),
        ];
        process_instruction(
            &ix.data,
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::MissingAccount),
        );
    }

    #[test]
    fn fail_execute_task_with_invalid_instructions() {
        init_validator_authority_if_needed(Keypair::new());
        let payer = Pubkey::new_unique();
        let mut inner_ix = InstructionUtils::schedule_task_instruction(
            &payer,
            ScheduleTaskArgs {
                task_id: 0,
                execution_interval_millis: 1,
                iterations: 1,
                instructions: vec![InstructionUtils::noop_instruction(0)],
            },
        );

        for (signer, writable, pubkey, expected) in [
            (
                true,
                false,
                Pubkey::new_unique(),
                InstructionError::MissingRequiredSignature,
            ),
            (false, true, CRANK_SIGNER, InstructionError::Immutable),
            (
                false,
                false,
                validator_authority_id(),
                InstructionError::IncorrectAuthority,
            ),
        ] {
            inner_ix.accounts[0].is_signer = signer;
            inner_ix.accounts[0].is_writable = writable;
            inner_ix.accounts[0].pubkey = pubkey;
            let ix = InstructionUtils::execute_task_instruction(vec![
                inner_ix.clone()
            ]);

            let transaction_accounts = vec![
                (
                    validator_authority_id(),
                    AccountSharedData::new(0, 0, &system_program::id()),
                ),
                (
                    CRANK_SIGNER,
                    AccountSharedData::new(0, 0, &system_program::id()),
                ),
            ];

            process_instruction(
                &ix.data,
                transaction_accounts,
                ix.accounts,
                Err(expected),
            );
        }
    }
}
