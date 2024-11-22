use std::collections::HashMap;

use num_derive::{FromPrimitive, ToPrimitive};
use serde::{Deserialize, Serialize};
use sleipnir_core::magic_program::MAGIC_CONTEXT_PUBKEY;
use solana_sdk::{
    account::Account,
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
    mutate_accounts::set_account_mod_data, validator_authority,
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

    #[error("The account resolution for the provided key failed.")]
    AccountDataResolutionFailed,

    #[error("The account data for the provided key is missing both from in-memory and ledger storage.")]
    AccountDataMissing,

    #[error("Tried to persist data that could not be resolved.")]
    AttemptedToPersistUnresolvedData,

    #[error("Tried to persist data that was resolved from storage.")]
    AttemptedToPersistDataFromStorage,

    #[error("Encountered an error when persisting account modification data.")]
    FailedToPersistAccountModData,
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

impl From<(&Pubkey, &Account)> for AccountModification {
    fn from(
        (account_pubkey, account): (&Pubkey, &Account),
    ) -> AccountModification {
        AccountModification {
            pubkey: *account_pubkey,
            lamports: Some(account.lamports),
            owner: Some(account.owner),
            executable: Some(account.executable),
            data: Some(account.data.clone()),
            rent_epoch: Some(account.rent_epoch),
        }
    }
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
    /// This is the first part of scheduling a commit.
    /// A second transaction [SleipnirInstruction::AcceptScheduleCommits] has to run in order
    /// to finish scheduling the commit.
    ///
    /// # Account references
    /// - **0.**   `[WRITE, SIGNER]` Payer requesting the commit to be scheduled
    /// - **1.**   `[WRITE]`         Magic Context Account containing to which we store
    ///                              the scheduled commits
    /// - **2..n** `[]`              Accounts to be committed
    ScheduleCommit,

    /// This is the exact same instruction as [SleipnirInstruction::ScheduleCommit] except
    /// that the [ScheduledCommit] is flagged such that when accounts are committed, a request
    /// to undelegate them is included with the same transaction.
    /// Additionally the validator will refuse anymore transactions for the specific account
    /// since they are no longer considered delegated to it.
    ///
    /// This is the first part of scheduling a commit.
    /// A second transaction [SleipnirInstruction::AcceptScheduleCommits] has to run in order
    /// to finish scheduling the commit.
    ///
    /// # Account references
    /// - **0.**   `[WRITE, SIGNER]` Payer requesting the commit to be scheduled
    /// - **1.**   `[WRITE]`         Magic Context Account containing to which we store
    ///                              the scheduled commits
    /// - **2..n** `[]`              Accounts to be committed and undelegated
    ScheduleCommitAndUndelegate,

    /// Moves the scheduled commit from the MagicContext to the global scheduled commits
    /// map. This is the second part of scheduling a commit.
    ///
    /// It is run at the start of the slot to update the global scheduled commits map just
    /// in time for the validator to realize the commits right after.
    ///
    /// # Account references
    /// - **0.**  `[SIGNER]` Validator Authority
    /// - **1.**  `[WRITE]`  Magic Context Account containing the initially scheduled commits
    AcceptScheduleCommits,

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
            ScheduleCommitAndUndelegate => 2,
            AcceptScheduleCommits => 3,
            ScheduledCommitSent(_) => 4,
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

pub fn modify_accounts_instruction(
    account_modifications: Vec<AccountModification>,
) -> Instruction {
    let mut account_metas =
        vec![AccountMeta::new(validator_authority_id(), true)];
    let mut account_mods: HashMap<Pubkey, AccountModificationForInstruction> =
        HashMap::new();
    for account_modification in account_modifications {
        account_metas
            .push(AccountMeta::new(account_modification.pubkey, false));
        let account_mod_for_instruction = AccountModificationForInstruction {
            lamports: account_modification.lamports,
            owner: account_modification.owner,
            executable: account_modification.executable,
            data_key: account_modification.data.map(set_account_mod_data),
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
    let mut account_metas = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
    ];
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
// Schedule Commit and Undelegate
// -----------------
pub fn schedule_commit_and_undelegate(
    payer: &Keypair,
    pubkeys: Vec<Pubkey>,
    recent_blockhash: Hash,
) -> Transaction {
    let ix =
        schedule_commit_and_undelegate_instruction(&payer.pubkey(), pubkeys);
    into_transaction(payer, ix, recent_blockhash)
}

pub(crate) fn schedule_commit_and_undelegate_instruction(
    payer: &Pubkey,
    pdas: Vec<Pubkey>,
) -> Instruction {
    let mut account_metas = vec![
        AccountMeta::new(*payer, true),
        AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
    ];
    for pubkey in &pdas {
        account_metas.push(AccountMeta::new_readonly(*pubkey, true));
    }
    Instruction::new_with_bincode(
        crate::id(),
        &SleipnirInstruction::ScheduleCommitAndUndelegate,
        account_metas,
    )
}

// -----------------
// Accept Scheduled Commits
// -----------------
pub fn accept_scheduled_commits(recent_blockhash: Hash) -> Transaction {
    let ix = accept_scheduled_commits_instruction();
    into_transaction(&validator_authority(), ix, recent_blockhash)
}

pub(crate) fn accept_scheduled_commits_instruction() -> Instruction {
    let account_metas = vec![
        AccountMeta::new_readonly(validator_authority_id(), true),
        AccountMeta::new(MAGIC_CONTEXT_PUBKEY, false),
    ];
    Instruction::new_with_bincode(
        crate::id(),
        &SleipnirInstruction::AcceptScheduleCommits,
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
pub(crate) fn into_transaction(
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
