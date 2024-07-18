use std::collections::HashMap;

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

    /// Forces the provided account to be committed to chain regardless
    /// of the commit frequency of the validator or the delegated account
    /// itself
    ///
    /// # Account references
    ///  - **0.** `[WRITE, SIGNER]` Payer requesting the account to be committed
    ///  - **1.** `[]`              Account to commit
    TriggerCommit,

    /// Schedules the accounts provided via the instruction args to be committed.
    /// It should be invoked from the program whose PDA accounts are to be
    /// committed.
    ///
    /// # Account references
    /// - **0.**   `[WRITE, SIGNER]` Payer requesting the commit to be scheduled
    /// - **1.**   `[SIGNER]`        The program owning the accounts to be committed
    /// - **2.**   `[WRITE]`         Validator authority to which we escrow tx cost
    /// - **3..n** `[]`              Accounts to be committed
    ScheduleCommit(Vec<Pubkey>),
}

// -----------------
// ModifyAccounts
// -----------------
pub fn modify_accounts(
    keyed_account_mods: Vec<(Pubkey, AccountModification)>,
    recent_blockhash: Hash,
) -> Transaction {
    let ix = modify_accounts_instruction(keyed_account_mods);
    into_transaction(&validator_authority(), ix, recent_blockhash)
}

pub(crate) fn modify_accounts_instruction(
    keyed_account_mods: Vec<(Pubkey, AccountModification)>,
) -> Instruction {
    let mut account_metas =
        vec![AccountMeta::new(validator_authority_id(), true)];
    let mut account_mods: HashMap<Pubkey, AccountModificationForInstruction> =
        HashMap::new();
    for (pubkey, account_mod) in keyed_account_mods {
        account_metas.push(AccountMeta::new(pubkey, false));
        let data_key = account_mod.data.map(set_account_mod_data);
        let account_mod_for_instruction = AccountModificationForInstruction {
            lamports: account_mod.lamports,
            owner: account_mod.owner,
            executable: account_mod.executable,
            data_key,
            rent_epoch: account_mod.rent_epoch,
        };
        account_mods.insert(pubkey, account_mod_for_instruction);
    }
    Instruction::new_with_bincode(
        crate::id(),
        &SleipnirInstruction::ModifyAccounts(account_mods),
        account_metas,
    )
}

// -----------------
// TriggerCommit
// -----------------
pub fn trigger_commit(
    payer: &Keypair,
    account_to_commit: Pubkey,
    recent_blockhash: Hash,
) -> Transaction {
    let ix = trigger_commit_instruction(payer, account_to_commit);
    into_transaction(payer, ix, recent_blockhash)
}

pub(crate) fn trigger_commit_instruction(
    payer: &Keypair,
    account_to_commit: Pubkey,
) -> Instruction {
    Instruction::new_with_bincode(
        crate::id(),
        &SleipnirInstruction::TriggerCommit,
        vec![
            AccountMeta::new(payer.pubkey(), true),
            AccountMeta::new_readonly(account_to_commit, false),
        ],
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
