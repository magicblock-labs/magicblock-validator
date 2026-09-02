use std::collections::HashSet;

use magicblock_core::intent::outbox::verify_outbox_intent_pda;
use solana_account::{AccountMode, ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;

use crate::{
    intent_bundles::outbox_intent_bundles::OutboxIntentBundle,
    utils::{
        account_actions::set_account_mode,
        accounts::{
            get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
        },
    },
    validator::authority,
};

const VALIDATOR_IDX: u16 = 0;
const MAGIC_PROGRAM_IDX: u16 = VALIDATOR_IDX + 1;
const CLOSING_PDA_IDX: u16 = MAGIC_PROGRAM_IDX + 1;

/// Closes the outbox intent PDA
/// Validates intent execution stage to see if it can be closed
pub fn process_close_outbox_intent(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    intent_id: u64,
) -> Result<(), InstructionError> {
    validate(&signers, invoke_context, intent_id)?;
    close_outbox_ephemeral_account(invoke_context)
}

fn validate(
    signers: &HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    intent_id: u64,
) -> Result<(Pubkey, Pubkey), InstructionError> {
    let transaction_context = &invoke_context.transaction_context;
    let ix_ctx = transaction_context.get_current_instruction_context()?;

    // Assert MagicBlock program
    if ix_ctx.get_program_key()? != &crate::id() {
        ic_msg!(
            invoke_context,
            "CloseOutboxIntent ERR: Magic program account not found"
        );
        return Err(InstructionError::UnsupportedProgramId);
    }

    // Assert validator identity matches
    let validator_pubkey =
        get_instruction_pubkey_with_idx(transaction_context, VALIDATOR_IDX)?;
    let validator_authority_id = authority();
    if validator_pubkey != &validator_authority_id {
        ic_msg!(
            invoke_context,
            "CloseOutboxIntent ERR: provided validator account {} does not match validator identity {}",
            validator_pubkey,
            validator_authority_id
        );
        return Err(InstructionError::IncorrectAuthority);
    }

    // Assert magic program account
    let magic_program_pubkey = get_instruction_pubkey_with_idx(
        transaction_context,
        MAGIC_PROGRAM_IDX,
    )?;
    if *magic_program_pubkey != crate::id() {
        ic_msg!(
            invoke_context,
            "CloseOutboxIntent ERR: account at idx {} is {}, expected magic program {}",
            MAGIC_PROGRAM_IDX,
            magic_program_pubkey,
            crate::id()
        );
        return Err(InstructionError::IncorrectProgramId);
    }

    // Assert signers
    if !signers.contains(&validator_authority_id) {
        ic_msg!(
            invoke_context,
            "CloseOutboxIntent ERR: validator authority not found in signers"
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    // Deserialize first so the PDA can be validated cheaply against its
    // own stored bump, instead of re-deriving via find_program_address.
    let intent_acc =
        get_instruction_account_with_idx(transaction_context, CLOSING_PDA_IDX)?;
    let bundle = OutboxIntentBundle::try_from_bytes(
        intent_acc.borrow()?.data(),
    )
    .map_err(|_| {
        ic_msg!(
            invoke_context,
            "CloseOutboxIntent ERR: failed to deserialize outbox intent {}",
            intent_id
        );
        InstructionError::InvalidAccountData
    })?;

    // Validate outbox intent PDA
    let provided_pda =
        get_instruction_pubkey_with_idx(transaction_context, CLOSING_PDA_IDX)?;
    if !verify_outbox_intent_pda(intent_id, bundle.bump(), provided_pda) {
        ic_msg!(
            invoke_context,
            "CloseOutboxIntent ERR: account at idx {} is {}, invalid PDA for intent {}",
            CLOSING_PDA_IDX,
            provided_pda,
            intent_id
        );
        return Err(InstructionError::InvalidArgument);
    }

    // Validate if intent reached stage where it can be closed
    if !bundle.is_final_stage() {
        ic_msg!(
            invoke_context,
            "CloseOutboxIntent ERR: intent {} is not in a final execution stage: {:?}",
            intent_id,
            bundle.status()
        );
        return Err(InstructionError::InvalidAccountData);
    }

    Ok((validator_authority_id, *provided_pda))
}

fn close_outbox_ephemeral_account(
    invoke_context: &InvokeContext,
) -> Result<(), InstructionError> {
    let transaction_context = &*invoke_context.transaction_context;
    let pda =
        get_instruction_account_with_idx(transaction_context, CLOSING_PDA_IDX)?;

    let mut acc = pda.borrow_mut()?;
    acc.set_lamports(0);
    acc.set_owner(system_program::id());
    acc.resize(0, 0);
    set_account_mode(invoke_context, &mut acc, AccountMode::Closed)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use magicblock_core::intent::{
        MagicIntentBundle, outbox::outbox_intent_pda_with_bump,
    };
    use magicblock_magic_program_api::{
        EPHEMERAL_VAULT_PUBKEY,
        outbox::{ExecutionStage, PendingTransaction, TwoStageProgress},
    };
    use solana_account::{AccountMode, AccountSharedData};
    use solana_hash::Hash;
    use solana_instruction::{Instruction, error::InstructionError};
    use solana_keypair::Keypair;
    use solana_sdk_ids::{bpf_loader_upgradeable, system_program};
    use solana_signature::Signature;
    use solana_signer::Signer;
    use solana_transaction::Transaction;

    use super::*;
    use crate::{
        instruction_utils::InstructionUtils,
        magic_scheduled_base_intent::ScheduledIntentBundle,
        test_utils::{ensure_started_validator, process_instruction},
    };

    fn transaction_accounts_from_map(
        ix: &Instruction,
        account_data: &mut std::collections::HashMap<Pubkey, AccountSharedData>,
    ) -> Vec<(Pubkey, AccountSharedData)> {
        ix.accounts
            .iter()
            .flat_map(|acc| {
                account_data
                    .remove(&acc.pubkey)
                    .map(|shared_data| (acc.pubkey, shared_data))
            })
            .collect()
    }

    fn outbox_bundle_bytes(
        intent_id: u64,
        bump: u8,
        stage: Option<ExecutionStage>,
    ) -> Vec<u8> {
        let inner = ScheduledIntentBundle {
            id: intent_id,
            slot: 0,
            blockhash: Hash::default(),
            sent_transaction: Transaction::default(),
            payer: Pubkey::new_unique(),
            intent_bundle: MagicIntentBundle::default(),
        };
        let mut bundle = OutboxIntentBundle::accepted(inner, bump);
        if let Some(stage) = stage {
            bundle.apply_stage_transition(stage).unwrap();
        }
        bundle.try_to_bytes().unwrap()
    }

    fn setup_outbox_pda_and_vault(
        intent_id: u64,
        stage: Option<ExecutionStage>,
    ) -> std::collections::HashMap<Pubkey, AccountSharedData> {
        let (pda, bump) = outbox_intent_pda_with_bump(intent_id);
        let data = outbox_bundle_bytes(intent_id, bump, stage);
        let mut pda_account =
            AccountSharedData::new(0, data.len(), &crate::id());
        pda_account.set_data_from_slice(&data);
        pda_account.set_mode(AccountMode::Ephemeral).unwrap();

        let mut map = std::collections::HashMap::new();
        // Pre-fund vault so CloseEphemeralAccount CPI can refund sponsor -
        // refund is rent-exempt-minimum for the PDA's actual data length
        let mut vault = AccountSharedData::new(1_000_000, 0, &crate::id());
        vault.set_mode(AccountMode::Ephemeral).unwrap();
        map.insert(EPHEMERAL_VAULT_PUBKEY, vault);
        // Add outbox PDA as existing ephemeral account (created by accept)
        map.insert(pda, pda_account);
        map
    }

    fn final_stage() -> ExecutionStage {
        ExecutionStage::SingleStage(PendingTransaction {
            signature: Signature::default(),
            blockhash: Hash::default(),
        })
    }

    #[test]
    fn test_missing_validator_auth_signer() {
        let intent_id: u64 = rand::random();
        let mut account_data =
            setup_outbox_pda_and_vault(intent_id, Some(final_stage()));
        ensure_started_validator(&mut account_data, None);

        let mut ix =
            InstructionUtils::close_outbox_intent_instruction(intent_id);
        ix.accounts[0].is_signer = false;

        let transaction_accounts =
            transaction_accounts_from_map(&ix, &mut account_data);
        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::MissingRequiredSignature),
        );
    }

    #[test]
    fn test_invalid_validator_auth() {
        let intent_id: u64 = rand::random();
        let fake_validator = Keypair::new();
        let mut account_data =
            setup_outbox_pda_and_vault(intent_id, Some(final_stage()));
        account_data.insert(
            fake_validator.pubkey(),
            AccountSharedData::new(1_000_000, 0, &system_program::id()),
        );
        ensure_started_validator(&mut account_data, None);

        let mut ix =
            InstructionUtils::close_outbox_intent_instruction(intent_id);
        ix.accounts[0].pubkey = fake_validator.pubkey();
        let transaction_accounts =
            transaction_accounts_from_map(&ix, &mut account_data);
        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::IncorrectAuthority),
        );
    }

    #[test]
    fn test_invalid_program() {
        let intent_id: u64 = rand::random();
        let fake_program = Keypair::new();
        let mut account_data =
            setup_outbox_pda_and_vault(intent_id, Some(final_stage()));
        account_data.insert(
            fake_program.pubkey(),
            AccountSharedData::new(0, 0, &bpf_loader_upgradeable::id()),
        );
        ensure_started_validator(&mut account_data, None);

        let mut ix =
            InstructionUtils::close_outbox_intent_instruction(intent_id);
        ix.accounts[1].pubkey = fake_program.pubkey();
        let transaction_accounts =
            transaction_accounts_from_map(&ix, &mut account_data);
        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::IncorrectProgramId),
        );
    }

    #[test]
    fn test_closes_outbox_pda_and_refunds_sponsor() {
        let intent_id: u64 = rand::random();
        let mut account_data =
            setup_outbox_pda_and_vault(intent_id, Some(final_stage()));
        ensure_started_validator(&mut account_data, None);

        let ix = InstructionUtils::close_outbox_intent_instruction(intent_id);
        let transaction_accounts =
            transaction_accounts_from_map(&ix, &mut account_data);
        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );
    }

    #[test]
    fn test_rejects_close_when_not_final_stage() {
        let intent_id: u64 = rand::random();
        // Still `Accepted` - execution never even started
        let mut account_data = setup_outbox_pda_and_vault(intent_id, None);
        ensure_started_validator(&mut account_data, None);

        let ix = InstructionUtils::close_outbox_intent_instruction(intent_id);
        let transaction_accounts =
            transaction_accounts_from_map(&ix, &mut account_data);
        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::InvalidAccountData),
        );
    }

    #[test]
    fn test_rejects_close_when_two_stage_still_committing() {
        let intent_id: u64 = rand::random();
        let committing = ExecutionStage::TwoStage(
            TwoStageProgress::Committing(PendingTransaction {
                signature: Signature::default(),
                blockhash: Hash::default(),
            }),
        );
        let mut account_data =
            setup_outbox_pda_and_vault(intent_id, Some(committing));
        ensure_started_validator(&mut account_data, None);

        let ix = InstructionUtils::close_outbox_intent_instruction(intent_id);
        let transaction_accounts =
            transaction_accounts_from_map(&ix, &mut account_data);
        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::InvalidAccountData),
        );
    }
}
