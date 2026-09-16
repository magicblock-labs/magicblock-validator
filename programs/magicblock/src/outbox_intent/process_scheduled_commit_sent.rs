use std::{
    collections::{HashMap, HashSet},
    sync::RwLock,
};

use lazy_static::lazy_static;
use magicblock_core::intent::outbox::verify_outbox_intent_pda;
use solana_account::ReadableAccount;
use solana_clock::Slot;
use solana_hash::Hash;
use solana_instruction::error::InstructionError;
use solana_log_collector::ic_msg;
use solana_program_runtime::invoke_context::InvokeContext;
use solana_pubkey::Pubkey;
use solana_signature::Signature;

use crate::{
    errors::custom_error_codes,
    outbox_intent::outbox_intent_bundles::OutboxIntentBundle,
    utils::accounts::{
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
    },
    validator::authority,
};

/// Error code returned when an intent execution failed.
/// This indicates the intent could not be successfully executed despite patching attempts.
const INTENT_FAILED_CODE: u32 = 0x7461636F;

#[derive(Default, Debug, Clone)]
pub struct SentCommit {
    pub message_id: u64,
    pub slot: Slot,
    pub blockhash: Hash,
    pub payer: Pubkey,
    pub chain_signatures: Vec<Signature>,
    pub included_pubkeys: Vec<Pubkey>,
    pub excluded_pubkeys: Vec<Pubkey>,
    pub requested_undelegation: bool,
    pub error_message: Option<String>,
    pub patched_errors: Vec<String>,
    /// Callbacks scheduling results: either the scheduled transaction signature
    /// or a compilation/scheduling error.
    pub callbacks_scheduling_results: Vec<String>,
}

/// This is a printable version of the SentCommit struct.
/// We prepare this outside of the VM in order to reduce overhead there.
#[derive(Debug, Clone)]
struct SentCommitPrintable {
    id: u64,
    slot: Slot,
    blockhash: String,
    payer: String,
    chain_signatures: Vec<String>,
    included_pubkeys: String,
    excluded_pubkeys: String,
    requested_undelegation: bool,
    error_message: Option<String>,
    patched_errors: Vec<String>,
    callbacks_scheduling_results: Vec<String>,
}

impl From<SentCommit> for SentCommitPrintable {
    fn from(commit: SentCommit) -> Self {
        Self {
            id: commit.message_id,
            slot: commit.slot,
            blockhash: commit.blockhash.to_string(),
            payer: commit.payer.to_string(),
            chain_signatures: commit
                .chain_signatures
                .iter()
                .map(|x| x.to_string())
                .collect(),
            included_pubkeys: commit
                .included_pubkeys
                .iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            excluded_pubkeys: commit
                .excluded_pubkeys
                .iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            requested_undelegation: commit.requested_undelegation,
            error_message: commit.error_message,
            patched_errors: commit.patched_errors,
            callbacks_scheduling_results: commit.callbacks_scheduling_results,
        }
    }
}

lazy_static! {
    // We need to determine the transaction signature before we even know the
    // signature of the transaction we are sending to chain and we don't know
    // what Pubkeys we will include before hand either.
    // Therefore the transaction itself only includes the ID of the scheduled
    // commit and we store the signature in a globally accessible hashmap.
    static ref SENT_COMMITS: RwLock<HashMap<u64, SentCommitPrintable>> = RwLock::new(HashMap::new());
}

pub fn register_scheduled_commit_sent(commit: SentCommit) {
    let id = commit.message_id;
    SENT_COMMITS
        .write()
        .expect("SENT_COMMITS lock poisoned")
        .insert(id, commit.into());
}

#[cfg(test)]
fn get_scheduled_commit(id: u64) -> Option<SentCommitPrintable> {
    SENT_COMMITS.read().unwrap().get(&id).cloned()
}

pub fn process_scheduled_commit_sent(
    signers: HashSet<Pubkey>,
    invoke_context: &mut InvokeContext,
    intent_id: u64,
) -> Result<(), InstructionError> {
    // No replica gating here: on a replica this will fail (the commit was
    // never registered in its local SENT_COMMITS map), but that's fine since
    // this instruction only clears bookkeeping state, not AccountsDB - it has
    // no effect on the replicated account state a replica needs to converge on.
    validate(&signers, invoke_context, intent_id)?;

    // Only after we passed all checks do we remove the commit from the global hashmap
    // Otherwise a malicious actor could remove a commit from the hashmap without
    // signing as the validator
    let commit = match SENT_COMMITS.write() {
        Ok(mut commits) => match commits.remove(&intent_id) {
            Some(commit) => commit,
            None => {
                ic_msg!(
                    invoke_context,
                    "ScheduleCommitSent ERR: commit with id {} not found",
                    intent_id
                );
                return Err(InstructionError::Custom(
                    custom_error_codes::CANNOT_FIND_SCHEDULED_COMMIT,
                ));
            }
        },
        Err(err) => {
            ic_msg!(
                invoke_context,
                "ScheduleCommitSent ERR: failed to lock SENT_COMMITS: {}",
                err
            );
            return Err(InstructionError::Custom(
                custom_error_codes::UNABLE_TO_UNLOCK_SENT_COMMITS,
            ));
        }
    };

    // Log data
    log_sent_commit(invoke_context, &commit);
    commit.error_message.map_or(Ok(()), |_| {
        Err(InstructionError::Custom(INTENT_FAILED_CODE))
    })
}

fn validate(
    signers: &HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    intent_id: u64,
) -> Result<(), InstructionError> {
    const VALIDATOR_IDX: u16 = 0;
    const CLOSING_PDA_IDX: u16 = VALIDATOR_IDX + 1;

    let transaction_context = &invoke_context.transaction_context;
    let ix_ctx = transaction_context.get_current_instruction_context()?;

    // Assert outbox intent program
    if ix_ctx.get_program_key()?
        != &magicblock_magic_program_api::OUTBOX_INTENT_PROGRAM_ID
    {
        ic_msg!(
            invoke_context,
            "ScheduleCommitSent ERR: outbox intent program account not found"
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
    let intent_acc =
        get_instruction_account_with_idx(transaction_context, CLOSING_PDA_IDX)?;
    let bundle = OutboxIntentBundle::try_from_bytes(
        intent_acc.borrow()?.data(),
    )
    .map_err(|_| {
        ic_msg!(
            invoke_context,
            "ScheduleCommitSent ERR: failed to deserialize outbox intent {}",
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
            "ScheduleCommitSent ERR: account at idx {} is {}, invalid PDA for intent {}",
            CLOSING_PDA_IDX,
            provided_pda,
            intent_id
        );
        return Err(InstructionError::InvalidArgument);
    }

    Ok(())
}

fn log_sent_commit(
    invoke_context: &InvokeContext,
    commit: &SentCommitPrintable,
) {
    ic_msg!(
        invoke_context,
        "ScheduledCommitSent id: {}, slot: {}, blockhash: {}",
        commit.id,
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
        commit.included_pubkeys,
    );
    ic_msg!(
        invoke_context,
        "ScheduledCommitSent excluded: [{}]",
        commit.excluded_pubkeys
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
    for (idx, report) in commit.callbacks_scheduling_results.iter().enumerate()
    {
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
    use magicblock_core::intent::{
        MagicIntentBundle, outbox::outbox_intent_pda_with_bump,
    };
    use magicblock_magic_program_api::OUTBOX_INTENT_PROGRAM_ID;
    use solana_account::{AccountBuilder, AccountMode, AccountSharedData};
    use solana_instruction::{Instruction, error::InstructionError};
    use solana_keypair::Keypair;
    use solana_sdk_ids::system_program;
    use solana_signer::Signer;
    use solana_transaction::Transaction;

    use super::*;
    use crate::{
        instruction_utils::InstructionUtils,
        magic_scheduled_base_intent::ScheduledIntentBundle,
        test_utils::{
            ensure_started_validator, process_outbox_intent_instruction,
        },
    };

    fn single_acc_commit(commit_id: u64) -> SentCommit {
        let slot = 10;
        let sig = Signature::default();
        let payer = Pubkey::new_unique();
        let acc = Pubkey::new_unique();
        SentCommit {
            message_id: commit_id,
            slot,
            blockhash: Hash::default(),
            payer,
            chain_signatures: vec![sig],
            included_pubkeys: vec![acc],
            excluded_pubkeys: Default::default(),
            requested_undelegation: false,
            error_message: None,
            patched_errors: vec![],
            callbacks_scheduling_results: vec![],
        }
    }

    fn transaction_accounts_from_map(
        ix: &Instruction,
        account_data: &mut HashMap<Pubkey, AccountSharedData>,
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

    fn setup_registered_commit() -> SentCommit {
        let id: u64 = rand::random();
        let commit = single_acc_commit(id);
        register_scheduled_commit_sent(commit.clone());
        commit
    }

    #[test]
    fn test_registered_but_missing_validator_auth_signer() {
        let commit = setup_registered_commit();

        let mut account_data = HashMap::new();

        ensure_started_validator(&mut account_data, None);

        let mut ix = InstructionUtils::scheduled_commit_sent_instruction(
            commit.message_id,
        );
        ix.accounts[0].is_signer = false;

        let transaction_accounts =
            transaction_accounts_from_map(&ix, &mut account_data);
        process_outbox_intent_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::MissingRequiredSignature),
        );

        assert!(
            get_scheduled_commit(commit.message_id).is_some(),
            "does not remove scheduled commit data"
        );
    }

    #[test]
    fn test_registered_but_invalid_validator_auth() {
        let commit = setup_registered_commit();

        let fake_validator = Keypair::new();
        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(
                fake_validator.pubkey(),
                AccountSharedData::new(1_000_000, 0, &system_program::id()),
            );
            map
        };
        ensure_started_validator(&mut account_data, None);

        let mut ix = InstructionUtils::scheduled_commit_sent_instruction(
            commit.message_id,
        );
        ix.accounts[0].pubkey = fake_validator.pubkey();
        let transaction_accounts =
            transaction_accounts_from_map(&ix, &mut account_data);
        process_outbox_intent_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::IncorrectAuthority),
        );

        assert!(
            get_scheduled_commit(commit.message_id).is_some(),
            "does not remove scheduled commit data"
        );
    }

    #[test]
    fn test_registered_all_checks_out() {
        let commit = setup_registered_commit();

        let (pda, bump) = outbox_intent_pda_with_bump(commit.message_id);
        let bundle = OutboxIntentBundle::accepted(
            ScheduledIntentBundle {
                id: commit.message_id,
                slot: 0,
                blockhash: Hash::default(),
                sent_transaction: Transaction::default(),
                payer: Pubkey::new_unique(),
                intent_bundle: MagicIntentBundle::default(),
            },
            bump,
        );
        let data = bundle.try_to_bytes().unwrap();
        let mut pda_account = AccountBuilder::from(AccountSharedData::new(
            0,
            data.len(),
            &OUTBOX_INTENT_PROGRAM_ID,
        ))
        .mode(AccountMode::Ephemeral)
        .build::<AccountSharedData>();
        pda_account.set_data_from_slice(&data);

        let mut account_data = {
            let mut map = HashMap::new();
            // Add outbox PDA as existing ephemeral account (created by accept)
            map.insert(pda, pda_account);
            map
        };

        ensure_started_validator(&mut account_data, None);

        let ix = InstructionUtils::scheduled_commit_sent_instruction(
            commit.message_id,
        );

        let transaction_accounts =
            transaction_accounts_from_map(&ix, &mut account_data);
        process_outbox_intent_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );

        assert!(
            get_scheduled_commit(commit.message_id).is_none(),
            "removes scheduled commit data"
        );
    }
}
