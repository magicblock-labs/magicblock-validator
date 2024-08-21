use std::{collections::HashMap, vec};

use num_derive::{FromPrimitive, ToPrimitive};
use serde::{Deserialize, Serialize};
use solana_sdk::{
    decode_error::DecodeError,
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::Transaction,
};
use thiserror::Error;

use crate::{
    sleipnir_processor::set_account_mod_data, validator_authority,
    validator_authority_id,
};

#[derive(
    Error, Debug, Serialize, Clone, PartialEq, Eq, FromPrimitive, ToPrimitive,
)]
pub enum SleipnirError {
    #[error("need at least one account to modify")]
    NoAccountsToModify,

    #[error("number of accounts to modify needs to match number of account modifications")]
    AccountsToModifyNotMatchingAccountModifications,

    #[error("The account modification for the provided key is missing.")]
    AccountModificationMissing,

    #[error("first account needs to be Sleipnir authority")]
    FirstAccountNeedsToBeSleipnirAuthority,

    #[error("Sleipnir authority needs to be owned by system program")]
    SleipnirAuthorityNeedsToBeOwnedBySystemProgram,

    #[error("The account data for the provided key is missing.")]
    AccountDataMissing,
}

impl<T> DecodeError<T> for SleipnirError {
    fn type_of() -> &'static str {
        "SleipnirError"
    }
}

#[derive(Default, Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct AccountModification {
    pub pubkey: Pubkey,
    pub lamports: Option<u64>,
    pub owner: Option<Pubkey>,
    pub executable: Option<bool>,
    pub data: Option<Vec<u8>>,
    pub rent_epoch: Option<u64>,
}

#[derive(Default, Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub(crate) struct AccountModificationForInstruction {
    pub lamports: Option<u64>,
    pub owner: Option<Pubkey>,
    pub executable: Option<bool>,
    pub data_key: Option<usize>,
    pub rent_epoch: Option<u64>,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub(crate) enum SleipnirInstruction {
    /// Modify one or more accounts
    ///
    /// # Account references
    ///  - **0.**    `[WRITE, SIGNER]` Validator Authority
    ///  - **1..n.** `[WRITE]` Accounts to modify
    ///  - **n+1**  `[SIGNER]` (Implicit NativeLoader)
    ModifyAccounts(HashMap<Pubkey, AccountModificationForInstruction>),

    /// Schedules the accounts provided at end of accounts Vec to be committed.
    /// It should be invoked from the program whose PDA accounts are to be
    /// committed.
    ///
    /// # Account references
    /// - **0.**   `[WRITE, SIGNER]` Payer requesting the commit to be scheduled
    /// - **1..n** `[]`              Accounts to be committed
    ScheduleCommit,

    /// Records the the attempt to realize a scheduled commit on chain.
    ///
    /// The signature of this transaction can be pre-calculated since we pass the
    /// ID of the scheduled commit and retrieve the signature from a globally
    /// stored hashmap.
    ///
    /// We implement it this way so we can log the signature of this transaction
    /// as part of the [SleipnirInstruction::ScheduleCommit] instruction.
    ScheduledCommitSent(u64),
}

#[allow(unused)]
impl SleipnirInstruction {
    pub(crate) fn index(&self) -> u8 {
        use SleipnirInstruction::*;
        match self {
            ModifyAccounts(_) => 0,
            ScheduleCommit => 1,
            ScheduledCommitSent(_) => 2,
        }
    }

    pub(crate) fn discriminant(&self) -> [u8; 4] {
        let idx = self.index();
        [idx, 0, 0, 0]
    }

    pub(crate) fn try_to_vec(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }
}

// -----------------
// ModifyAccounts
// -----------------
pub fn modify_accounts(
    account_modifications: Vec<AccountModification>,
    recent_blockhash: Hash,
) -> Transaction {
    let ix = modify_accounts_instruction(account_modifications);
    into_transaction(&validator_authority(), ix, recent_blockhash)
}

pub(crate) fn modify_accounts_instruction(
    account_modifications: Vec<AccountModification>,
) -> Instruction {
    let mut account_metas =
        vec![AccountMeta::new(validator_authority_id(), true)];
    let mut account_mods: HashMap<Pubkey, AccountModificationForInstruction> =
        HashMap::new();
    for account_modification in account_modifications {
        account_metas
            .push(AccountMeta::new(account_modification.pubkey, false));
        let data_key = account_modification.data.map(set_account_mod_data);
        let account_mod_for_instruction = AccountModificationForInstruction {
            lamports: account_modification.lamports,
            owner: account_modification.owner,
            executable: account_modification.executable,
            data_key,
            rent_epoch: account_modification.rent_epoch,
        };
        account_mods
            .insert(account_modification.pubkey, account_mod_for_instruction);
    }
    Instruction::new_with_bincode(
        crate::id(),
        &SleipnirInstruction::ModifyAccounts(account_mods),
        account_metas,
    )
}

// -----------------
// Schedule Commit
// -----------------
pub fn schedule_commit(
    payer: &Keypair,
    pubkeys: Vec<Pubkey>,
    recent_blockhash: Hash,
) -> Transaction {
    let ix = schedule_commit_instruction(&payer.pubkey(), pubkeys);
    into_transaction(payer, ix, recent_blockhash)
}

pub(crate) fn schedule_commit_instruction(
    payer: &Pubkey,
    pdas: Vec<Pubkey>,
) -> Instruction {
    let mut account_metas = vec![AccountMeta::new(*payer, true)];
    for pubkey in &pdas {
        account_metas.push(AccountMeta::new_readonly(*pubkey, true));
    }
    Instruction::new_with_bincode(
        crate::id(),
        &SleipnirInstruction::ScheduleCommit,
        account_metas,
    )
}

// -----------------
// Scheduled Commit Sent
// -----------------
pub fn scheduled_commit_sent(
    scheduled_commit_id: u64,
    recent_blockhash: Hash,
) -> Transaction {
    let ix = scheduled_commit_sent_instruction(
        &crate::id(),
        &validator_authority_id(),
        scheduled_commit_id,
    );
    into_transaction(&validator_authority(), ix, recent_blockhash)
}

pub(crate) fn scheduled_commit_sent_instruction(
    magic_block_program: &Pubkey,
    validator_authority: &Pubkey,
    scheduled_commit_id: u64,
) -> Instruction {
    let account_metas = vec![
        AccountMeta::new_readonly(*magic_block_program, false),
        AccountMeta::new_readonly(*validator_authority, true),
    ];
    Instruction::new_with_bincode(
        *magic_block_program,
        &SleipnirInstruction::ScheduledCommitSent(scheduled_commit_id),
        account_metas,
    )
}

// -----------------
// Utils
// -----------------
fn into_transaction(
    signer: &Keypair,
    instruction: Instruction,
    recent_blockhash: Hash,
) -> Transaction {
    let signers = &[&signer];
    Transaction::new_signed_with_payer(
        &[instruction],
        Some(&signer.pubkey()),
        signers,
        recent_blockhash,
    )
}
