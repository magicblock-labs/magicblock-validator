use std::{
    collections::{HashMap, HashSet},
    sync::RwLock,
};

use lazy_static::lazy_static;
use solana_program_runtime::{ic_msg, invoke_context::InvokeContext};
use solana_sdk::{
    clock::Slot, hash::Hash, instruction::InstructionError, pubkey::Pubkey,
    signature::Signature, transaction_context::TransactionContext,
};

use crate::utils::accounts::get_instruction_pubkey_with_idx;

pub struct SentCommit {
    id: u64,
    slot: Slot,
    blockhash: Hash,
    payer: Pubkey,
    chain_signatures: Vec<Signature>,
    included_pubkeys: Vec<Pubkey>,
    excluded_pubkeys: Vec<Pubkey>,
}

impl SentCommit {
    pub fn new(
        id: u64,
        slot: Slot,
        blockhash: Hash,
        payer: Pubkey,
        chain_signatures: Vec<Signature>,
        included_pubkeys: Vec<Pubkey>,
        excluded_pubkeys: Vec<Pubkey>,
    ) -> Self {
        Self {
            id,
            slot,
            blockhash,
            payer,
            chain_signatures,
            included_pubkeys,
            excluded_pubkeys,
        }
    }
}

/// This is a printable version of the SentCommit struct.
/// We prepare this outside of the VM in order to reduce overhead there.
struct SentCommitPrintable {
    id: u64,
    slot: Slot,
    blockhash: String,
    payer: String,
    chain_signatures: Vec<String>,
    included_pubkeys: String,
    excluded_pubkeys: String,
}

impl From<SentCommit> for SentCommitPrintable {
    fn from(commit: SentCommit) -> Self {
        Self {
            id: commit.id,
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
                .collect(),
            excluded_pubkeys: commit
                .excluded_pubkeys
                .iter()
                .map(|x| x.to_string())
                .collect(),
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
    let id = commit.id;
    SENT_COMMITS
        .write()
        .expect("SENT_COMMITS lock poisoned")
        .insert(id, commit.into());
}

pub fn process_scheduled_commit_sent(
    signers: HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    id: u64,
) -> Result<(), InstructionError> {
    let commit = SENT_COMMITS
        .write()
        .expect("SENT_COMMITS lock poisoned")
        .remove(&id)
        .expect("Scheduled commit not found");

    const _PROGRAM_IDX: u16 = 0;
    const VALIDATOR_IDX: u16 = 1;

    let ix_ctx = transaction_context.get_current_instruction_context()?;

    // Assert MagicBlock program
    ix_ctx
        .find_index_of_program_account(transaction_context, &crate::id())
        .ok_or_else(|| {
            ic_msg!(
                invoke_context,
                "ScheduleCommit ERR: Magic program account not found"
            );
            InstructionError::UnsupportedProgramId
        })?;

    // Assert validator identity matches
    let validator_pubkey =
        get_instruction_pubkey_with_idx(transaction_context, VALIDATOR_IDX)?;
    let validator_authority_id = crate::validator_authority_id();
    if validator_pubkey != &validator_authority_id {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: provided validator account {} does not match validator identity {}",
            validator_pubkey, validator_authority_id
        );
        return Err(InstructionError::IncorrectAuthority);
    }

    // Assert signers
    if !signers.contains(&validator_authority_id) {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: validator authority not found in signers"
        );
        return Err(InstructionError::MissingRequiredSignature);
    }

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
            "ScheduledCommitSent signature[{}]: {:?}",
            idx,
            sig
        );
    }

    Ok(())
}
