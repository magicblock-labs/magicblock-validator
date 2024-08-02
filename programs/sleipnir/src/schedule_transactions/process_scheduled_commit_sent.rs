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

use crate::{
    errors::custom_error_codes,
    utils::accounts::get_instruction_pubkey_with_idx,
};

#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
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
                .collect::<Vec<_>>()
                .join(", "),
            excluded_pubkeys: commit
                .excluded_pubkeys
                .iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join(", "),
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

#[cfg(test)]
fn get_scheduled_commit(id: u64) -> Option<SentCommitPrintable> {
    SENT_COMMITS.read().unwrap().get(&id).cloned()
}

pub fn process_scheduled_commit_sent(
    signers: HashSet<Pubkey>,
    invoke_context: &InvokeContext,
    transaction_context: &TransactionContext,
    id: u64,
) -> Result<(), InstructionError> {
    const PROGRAM_IDX: u16 = 0;
    const VALIDATOR_IDX: u16 = 1;

    // Assert MagicBlock program
    let program_id =
        get_instruction_pubkey_with_idx(transaction_context, PROGRAM_IDX)?;
    if program_id.ne(&crate::id()) {
        ic_msg!(
            invoke_context,
            "ScheduleCommit ERR: Invalid program id '{}'",
            program_id
        );
        return Err(InstructionError::IncorrectProgramId);
    }

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

    // Only after we passed all checks do we remove the commit from the global hashmap
    // Otherwise a malicious actor could remove a commit from the hashmap without
    // signing as the validator
    let commit = match SENT_COMMITS.write() {
        Ok(mut commits) => match commits.remove(&id) {
            Some(commit) => commit,
            None => {
                ic_msg!(
                    invoke_context,
                    "ScheduleCommit ERR: commit with id {} not found",
                    id
                );
                return Err(InstructionError::Custom(
                    custom_error_codes::UNABLE_TO_UNLOCK_SENT_COMMITS,
                ));
            }
        },
        Err(err) => {
            ic_msg!(
                invoke_context,
                "ScheduleCommit ERR: failed to lock SENT_COMMITS: {}",
                err
            );
            return Err(InstructionError::Custom(
                custom_error_codes::CANNOT_FIND_SCHEDULED_COMMIT,
            ));
        }
    };

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

    Ok(())
}

#[cfg(test)]
mod tests {
    use solana_sdk::{
        account::AccountSharedData,
        bpf_loader_upgradeable,
        instruction::{Instruction, InstructionError},
        signature::Keypair,
        signer::Signer,
        system_program,
    };

    use super::*;
    use crate::{
        sleipnir_instruction::scheduled_commit_sent_instruction,
        test_utils::{ensure_funded_validator_authority, process_instruction},
        validator_authority_id,
    };

    fn single_acc_commit(id: u64) -> SentCommit {
        let slot = 10;
        let sig = Signature::default();
        let payer = Pubkey::new_unique();
        let acc = Pubkey::new_unique();
        SentCommit::new(
            id,
            slot,
            Hash::default(),
            payer,
            vec![sig],
            vec![acc],
            vec![],
        )
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

        ensure_funded_validator_authority(&mut account_data);

        let mut ix = scheduled_commit_sent_instruction(
            &crate::id(),
            &validator_authority_id(),
            commit.id,
        );
        ix.accounts[1].is_signer = false;

        let transaction_accounts =
            transaction_accounts_from_map(&ix, &mut account_data);
        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::MissingRequiredSignature),
        );

        assert!(
            get_scheduled_commit(commit.id).is_some(),
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
        ensure_funded_validator_authority(&mut account_data);

        let ix = scheduled_commit_sent_instruction(
            &crate::id(),
            &fake_validator.pubkey(),
            commit.id,
        );
        let transaction_accounts =
            transaction_accounts_from_map(&ix, &mut account_data);
        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::IncorrectAuthority),
        );

        assert!(
            get_scheduled_commit(commit.id).is_some(),
            "does not remove scheduled commit data"
        );
    }

    #[test]
    fn test_registered_but_invalid_program() {
        let commit = setup_registered_commit();

        let fake_program = Keypair::new();
        let mut account_data = {
            let mut map = HashMap::new();
            map.insert(
                fake_program.pubkey(),
                AccountSharedData::new(0, 0, &bpf_loader_upgradeable::id()),
            );
            map
        };
        ensure_funded_validator_authority(&mut account_data);

        let ix = scheduled_commit_sent_instruction(
            &fake_program.pubkey(),
            &validator_authority_id(),
            commit.id,
        );
        let transaction_accounts =
            transaction_accounts_from_map(&ix, &mut account_data);

        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Err(InstructionError::IncorrectProgramId),
        );

        assert!(
            get_scheduled_commit(commit.id).is_some(),
            "does not remove scheduled commit data"
        );
    }

    #[test]
    fn test_registered_all_checks_out() {
        let commit = setup_registered_commit();

        let mut account_data = HashMap::new();

        ensure_funded_validator_authority(&mut account_data);

        let ix = scheduled_commit_sent_instruction(
            &crate::id(),
            &validator_authority_id(),
            commit.id,
        );

        let transaction_accounts =
            transaction_accounts_from_map(&ix, &mut account_data);
        process_instruction(
            ix.data.as_slice(),
            transaction_accounts,
            ix.accounts,
            Ok(()),
        );

        assert!(
            get_scheduled_commit(commit.id).is_none(),
            "removes scheduled commit data"
        );
    }
}
