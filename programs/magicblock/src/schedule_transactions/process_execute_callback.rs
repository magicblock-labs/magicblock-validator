use std::collections::HashSet;

use magicblock_magic_program_api::{pda::CALLBACK_SIGNER, CALLBACK_PROGRAM_ID};
use solana_instruction::{error::InstructionError, Instruction};
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;

use crate::{
    schedule_transactions::validate_callback_accounts,
    utils::{
        accounts::get_instruction_pubkey_with_idx, native_invoke::NativeInvoke,
    },
    validator::validator_authority_id,
    Pubkey,
};

const VALIDATOR_IDX: u16 = 0;
const CALLBACK_SIGNER_IDX: u16 = 1;

/// Propagates callback defined by user
pub(crate) fn process_execute_callback(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    instruction: Instruction,
) -> Result<(), InstructionError> {
    validate(signers, invoke_context)?;
    validate_callback_accounts(
        &invoke_context,
        &instruction.accounts,
        "ExecuteCallback ERR",
    )?;

    invoke_context.native_invoke(instruction, &[CALLBACK_SIGNER])
}

/// Checks if callback is correctly authorized
/// 1. Signed by validator
/// 2. Contains necessary accounts
/// 3. Account presence required by inner instruction are checked by `invokeContext::native_invoke`
fn validate(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
) -> Result<(), InstructionError> {
    let transaction_context = &invoke_context.transaction_context;
    let ix_ctx = transaction_context.get_current_instruction_context()?;

    // Assert callback executor program.
    let program_key = ix_ctx.get_program_key()?;
    if program_key != &CALLBACK_PROGRAM_ID {
        ic_msg!(
            invoke_context,
            "ExecuteCallback ERR: callback executor program account not found"
        );
        return Err(InstructionError::UnsupportedProgramId);
    }

    // Assert Validator is signer
    // Only the validator can execute a callback
    let validator_pubkey =
        get_instruction_pubkey_with_idx(transaction_context, VALIDATOR_IDX)?;
    if validator_pubkey != &validator_authority_id() {
        ic_msg!(
            invoke_context,
            "ExecuteCallback ERR: validator pubkey {} is not the expected validator",
            validator_pubkey
        );
        return Err(InstructionError::IncorrectAuthority);
    }
    if !signers.contains(validator_pubkey) {
        ic_msg!(
            invoke_context,
            "ExecuteCallback ERR: validator pubkey {} is not in signers",
            validator_pubkey
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    // Assert Callback signer is provided
    let callback_signer_pubkey = get_instruction_pubkey_with_idx(
        transaction_context,
        CALLBACK_SIGNER_IDX,
    )?;
    if callback_signer_pubkey != &CALLBACK_SIGNER {
        ic_msg!(
            invoke_context,
            "ExecuteCallback ERR: callback signer pubkey {} is not the expected Callback signer",
            callback_signer_pubkey
        );
        return Err(InstructionError::InvalidSeeds);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use magicblock_magic_program_api::{
        instruction::CallbackInstruction, pda::CALLBACK_SIGNER,
        CALLBACK_PROGRAM_ID,
    };
    use serial_test::serial;
    use solana_account::AccountSharedData;
    use solana_instruction::{
        error::InstructionError, AccountMeta, Instruction,
    };
    use solana_program_runtime::invoke_context::mock_process_instruction;
    use solana_pubkey::Pubkey;

    use crate::{
        magicblock_processor::CallbackEntrypoint,
        validator::{
            generate_validator_authority_if_needed, validator_authority_id,
        },
    };

    fn make_data(inner_accounts: Vec<AccountMeta>) -> Vec<u8> {
        bincode::serialize(&CallbackInstruction::ExecuteCallback {
            instruction: Instruction {
                program_id: Pubkey::new_unique(),
                accounts: inner_accounts,
                data: vec![],
            },
        })
        .unwrap()
    }

    fn setup_validator() -> Pubkey {
        generate_validator_authority_if_needed();
        validator_authority_id()
    }

    fn outer_accounts(
        validator: Pubkey,
        callback_signer: Pubkey,
    ) -> (Vec<(Pubkey, AccountSharedData)>, Vec<AccountMeta>) {
        (
            vec![
                (validator, AccountSharedData::default()),
                (callback_signer, AccountSharedData::default()),
            ],
            vec![
                AccountMeta::new(validator, true),
                AccountMeta::new_readonly(callback_signer, false),
            ],
        )
    }

    fn run(
        data: &[u8],
        transaction_accounts: Vec<(Pubkey, AccountSharedData)>,
        instruction_accounts: Vec<AccountMeta>,
        expected: Result<(), InstructionError>,
    ) {
        mock_process_instruction(
            &CALLBACK_PROGRAM_ID,
            None,
            data,
            transaction_accounts,
            instruction_accounts,
            expected,
            CallbackEntrypoint::vm,
            |_| {},
            |_| {},
        );
    }

    #[test]
    #[serial]
    fn test_validate_rejects_validator_not_signer() {
        let validator = setup_validator();
        let data = make_data(vec![]);
        let (tx, _) = outer_accounts(validator, CALLBACK_SIGNER);
        run(
            &data,
            tx,
            vec![
                AccountMeta::new(validator, false), // not a signer
                AccountMeta::new_readonly(CALLBACK_SIGNER, false),
            ],
            Err(InstructionError::MissingRequiredSignature),
        );
    }

    #[test]
    #[serial]
    fn test_validate_rejects_wrong_callback_signer() {
        let validator = setup_validator();
        let wrong = Pubkey::new_unique();
        let data = make_data(vec![]);
        let (tx, _) = outer_accounts(validator, wrong);
        run(
            &data,
            tx,
            vec![
                AccountMeta::new(validator, true),
                AccountMeta::new_readonly(wrong, false), // wrong pubkey at CALLBACK_SIGNER_IDX
            ],
            Err(InstructionError::InvalidSeeds),
        );
    }

    // validate_callback_accounts() failures

    #[test]
    #[serial]
    fn test_callback_accounts_rejects_unauthorized_signer() {
        let validator = setup_validator();
        let (tx, ix) = outer_accounts(validator, CALLBACK_SIGNER);
        let rogue = Pubkey::new_unique();
        let data = make_data(vec![AccountMeta::new_readonly(rogue, true)]);
        run(&data, tx, ix, Err(InstructionError::IncorrectAuthority));
    }

    #[test]
    #[serial]
    fn test_callback_accounts_rejects_writable_callback_signer() {
        let validator = setup_validator();
        let (tx, ix) = outer_accounts(validator, CALLBACK_SIGNER);
        let data = make_data(vec![AccountMeta::new(CALLBACK_SIGNER, false)]);
        run(&data, tx, ix, Err(InstructionError::Immutable));
    }

    #[test]
    #[serial]
    fn test_callback_accounts_rejects_validator_authority() {
        let validator = setup_validator();
        let (tx, ix) = outer_accounts(validator, CALLBACK_SIGNER);
        let data = make_data(vec![AccountMeta::new_readonly(validator, false)]);
        run(&data, tx, ix, Err(InstructionError::IncorrectAuthority));
    }
}
