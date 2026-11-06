use std::collections::HashSet;

use magic_program_api::{OUTBOX_INTENT_PROGRAM_ID, outbox::SentCommit};
use mbv_core::intent::outbox::verify_outbox_intent_pda;
use solana_account::{AccountMode, ReadableAccount, WritableAccount};
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_sdk_ids::system_program;

use crate::{
    outbox_intent::outbox_intent_bundles::OutboxIntentBundle,
    utils::{
        account_actions::set_account_mode,
        accounts::{get_instruction_account_with_idx, get_instruction_pubkey_with_idx},
    },
    validator::authority,
};

const VALIDATOR_IDX: u16 = 0;
const CLOSING_PDA_IDX: u16 = VALIDATOR_IDX + 1;

pub(crate) fn process_scheduled_commit_sent(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext<'_, '_>,
    commit: SentCommit,
) -> Result<(), InstructionError> {
    validate(&signers, invoke_context, commit.message_id)?;
    close_outbox_ephemeral_account(invoke_context)?;
    log_sent_commit(invoke_context, &commit);
    Ok(())
}

fn validate(
    signers: &HashSet<Pubkey>,
    invoke_context: &InvokeContext<'_, '_>,
    intent_id: u64,
) -> Result<(), InstructionError> {
    let transaction_context = &invoke_context.transaction_context;
    let ix_ctx = transaction_context.get_current_instruction_context()?;

    // Assert outbox intent program
    if ix_ctx.get_program_key()? != &OUTBOX_INTENT_PROGRAM_ID {
        ic_msg!(
            invoke_context,
            "ScheduleCommitSent ERR: outbox intent program account not found"
        );
        return Err(InstructionError::UnsupportedProgramId);
    }

    // Assert validator identity matches
    let validator_pubkey = get_instruction_pubkey_with_idx(transaction_context, VALIDATOR_IDX)?;
    let validator_authority_id = authority();
    if validator_pubkey != &validator_authority_id {
        ic_msg!(
            invoke_context,
            "ScheduleCommitSent ERR: provided validator account {} does not match validator identity {}",
            validator_pubkey,
            validator_authority_id
        );
        return Err(InstructionError::IncorrectAuthority);
    }

    // Assert signers
    if !signers.contains(&validator_authority_id) {
        ic_msg!(
            invoke_context,
            "ScheduleCommitSent ERR: validator authority not found in signers"
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

    // Deserialize first so the PDA can be validated cheaply against its
    // own stored bump, instead of re-deriving via find_program_address.
    let intent_acc = get_instruction_account_with_idx(transaction_context, CLOSING_PDA_IDX)?;
    if !intent_acc.is_writable()? {
        return Err(InstructionError::ReadonlyDataModified);
    }
    let account = intent_acc.borrow()?;
    if account.owner() != &OUTBOX_INTENT_PROGRAM_ID {
        return Err(InstructionError::InvalidAccountOwner);
    }
    let bundle = OutboxIntentBundle::try_from_bytes(account.data()).map_err(|_| {
        ic_msg!(
            invoke_context,
            "ScheduleCommitSent ERR: failed to deserialize outbox intent {}",
            intent_id
        );
        InstructionError::InvalidAccountData
    })?;

    // Validate outbox intent PDA
    let provided_pda = get_instruction_pubkey_with_idx(transaction_context, CLOSING_PDA_IDX)?;
    if bundle.inner.intent_id != intent_id
        || !verify_outbox_intent_pda(intent_id, bundle.bump(), provided_pda)
    {
        ic_msg!(
            invoke_context,
            "ScheduleCommitSent ERR: account at idx {} is {}, invalid PDA for intent {}",
            CLOSING_PDA_IDX,
            provided_pda,
            intent_id
        );
        return Err(InstructionError::InvalidArgument);
    }

    Ok(())
}

fn close_outbox_ephemeral_account(
    invoke_context: &InvokeContext<'_, '_>,
) -> Result<(), InstructionError> {
    let transaction_context = &*invoke_context.transaction_context;
    let pda = get_instruction_account_with_idx(transaction_context, CLOSING_PDA_IDX)?;

    let mut acc = pda.borrow_mut()?;
    acc.set_lamports(0);
    acc.set_owner(system_program::id());
    acc.resize(0, 0);
    set_account_mode(invoke_context, &mut acc, AccountMode::Closed)?;

    Ok(())
}

fn log_sent_commit(invoke_context: &InvokeContext<'_, '_>, commit: &SentCommit) {
    ic_msg!(
        invoke_context,
        "ScheduledCommitSent id: {}, slot: {}, blockhash: {}",
        commit.message_id,
        commit.slot,
        commit.blockhash,
    );
    ic_msg!(
        invoke_context,
        "ScheduledCommitSent payer: {}",
        commit.payer
    );
    ic_msg!(
        invoke_context,
        "ScheduledCommitSent included: [{}]",
        commit
            .included_pubkeys
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
    );
    ic_msg!(
        invoke_context,
        "ScheduledCommitSent excluded: [{}]",
        commit
            .excluded_pubkeys
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    );
    for (idx, sig) in commit.chain_signatures.iter().enumerate() {
        ic_msg!(
            invoke_context,
            "ScheduledCommitSent signature[{}]: {}",
            idx,
            sig
        );
    }
    if commit.requested_undelegation {
        ic_msg!(invoke_context, "ScheduledCommitSent requested undelegation");
    }
    for (idx, error) in commit.patched_errors.iter().enumerate() {
        ic_msg!(
            invoke_context,
            "ScheduledCommitSent patched error[{}]: {}",
            idx,
            error
        );
    }
    for (idx, report) in commit.callbacks_scheduling_results.iter().enumerate() {
        ic_msg!(
            invoke_context,
            "ScheduledCommitSent callback[{}]: {}",
            idx,
            report
        );
    }
    if let Some(error_message) = &commit.error_message {
        ic_msg!(
            invoke_context,
            "ScheduledCommitSent error message: {}",
            error_message
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use mbv_core::intent::{MagicIntentBundle, outbox::outbox_intent_pda_with_bump};
    use solana_account::{AccountBuilder, AccountSharedData};
    use solana_hash::Hash;
    use solana_instruction::Instruction;
    use solana_signature::Signature;

    use super::*;
    use crate::{
        instruction_utils::InstructionUtils,
        magic_scheduled_base_intent::ScheduledIntentBundle,
        test_utils::{ensure_started_validator, process_outbox_intent_instruction_with_logs},
    };

    fn fixture() -> (SentCommit, Instruction, Vec<(Pubkey, AccountSharedData)>) {
        let commit = SentCommit {
            message_id: 42,
            slot: 10,
            blockhash: Hash::default(),
            payer: Pubkey::new_unique(),
            chain_signatures: vec![Signature::new_unique()],
            included_pubkeys: vec![Pubkey::new_unique()],
            requested_undelegation: true,
            patched_errors: vec!["recovered action error".into()],
            callbacks_scheduling_results: vec!["OK: callback signature".into()],
            ..Default::default()
        };
        let (pda, bump) = outbox_intent_pda_with_bump(commit.message_id);
        let bundle = OutboxIntentBundle::accepted(
            ScheduledIntentBundle {
                intent_id: commit.message_id,
                slot: commit.slot,
                blockhash: commit.blockhash,
                payer: commit.payer,
                intent_bundle: MagicIntentBundle::default(),
            },
            bump,
        );
        let account = AccountBuilder::default()
            .data(bundle.try_to_bytes().unwrap())
            .owner(OUTBOX_INTENT_PROGRAM_ID)
            .mode(AccountMode::Magic)
            .build();
        let mut accounts = HashMap::from([(pda, account)]);
        ensure_started_validator(&mut accounts, None);
        let ix = InstructionUtils::scheduled_commit_sent_instruction(authority(), commit.clone())
            .unwrap();
        let accounts = ix
            .accounts
            .iter()
            .map(|meta| (meta.pubkey, accounts.remove(&meta.pubkey).unwrap()))
            .collect();
        (commit, ix, accounts)
    }

    #[test]
    fn receipt_replay_preserves_logs_and_closes_outbox() {
        for error in [None, Some("base action failed".to_owned())] {
            let (mut commit, _, accounts) = fixture();
            commit.error_message = error;
            let ix =
                InstructionUtils::scheduled_commit_sent_instruction(authority(), commit.clone())
                    .unwrap();
            let execute = || {
                process_outbox_intent_instruction_with_logs(
                    &ix.data,
                    accounts.clone(),
                    ix.accounts.clone(),
                    Ok(()),
                )
            };
            let (updated, logs) = execute();
            // A replica starts from the same accounts and only needs the signed payload.
            assert_eq!(execute(), (updated.clone(), logs.clone()));
            assert!(updated[CLOSING_PDA_IDX as usize].is(AccountMode::Closed));
            assert!(updated[CLOSING_PDA_IDX as usize].data().is_empty());
            assert_eq!(
                updated[CLOSING_PDA_IDX as usize].owner(),
                &system_program::id()
            );
            assert!(logs.contains(&format!(
                "ScheduledCommitSent signature[0]: {}",
                commit.chain_signatures[0]
            )));
            assert!(logs.contains(&format!(
                "ScheduledCommitSent included: [{}]",
                commit.included_pubkeys[0]
            )));
            assert!(logs.contains(&"ScheduledCommitSent requested undelegation".to_string()));
            assert!(logs.contains(
                &"ScheduledCommitSent patched error[0]: recovered action error".to_string()
            ));
            assert!(
                logs.contains(
                    &"ScheduledCommitSent callback[0]: OK: callback signature".to_string()
                )
            );
            assert_eq!(
                logs.iter().any(|line| line.starts_with("ScheduledCommitSent error message:")),
                commit.error_message.is_some()
            );
        }
    }

    #[test]
    fn invalid_receipt_does_not_change_outbox() {
        enum Fault {
            MissingSigner,
            WrongAuthority,
            Readonly,
            WrongOwner,
            WrongPda,
            WrongIntent,
        }
        for fault in [
            Fault::MissingSigner,
            Fault::WrongAuthority,
            Fault::Readonly,
            Fault::WrongOwner,
            Fault::WrongPda,
            Fault::WrongIntent,
        ] {
            let (_, mut ix, mut accounts) = fixture();
            let expected = match fault {
                Fault::MissingSigner => {
                    ix.accounts[0].is_signer = false;
                    InstructionError::MissingRequiredSignature
                }
                Fault::WrongAuthority => {
                    let other = Pubkey::new_unique();
                    ix.accounts[0].pubkey = other;
                    accounts[0].0 = other;
                    InstructionError::IncorrectAuthority
                }
                Fault::Readonly => {
                    ix.accounts[1].is_writable = false;
                    InstructionError::ReadonlyDataModified
                }
                Fault::WrongOwner => {
                    accounts[1].1.set_owner(system_program::id());
                    InstructionError::InvalidAccountOwner
                }
                Fault::WrongPda => {
                    let other = Pubkey::new_unique();
                    ix.accounts[1].pubkey = other;
                    accounts[1].0 = other;
                    InstructionError::InvalidArgument
                }
                Fault::WrongIntent => {
                    let mut bundle =
                        OutboxIntentBundle::try_from_bytes(accounts[1].1.data()).unwrap();
                    bundle.inner.intent_id += 1;
                    accounts[1].1.set_data_from_slice(&bundle.try_to_bytes().unwrap());
                    InstructionError::InvalidArgument
                }
            };
            let initial = accounts[1].1.clone();
            let (updated, logs) = process_outbox_intent_instruction_with_logs(
                &ix.data,
                accounts,
                ix.accounts,
                Err(expected),
            );
            assert_eq!(updated[1], initial);
            assert!(!logs.iter().any(|line| line.starts_with("ScheduledCommitSent id:")));
        }
    }
}
