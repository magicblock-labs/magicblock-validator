use std::collections::HashMap;

use num_derive::{FromPrimitive, ToPrimitive};
use serde::{Deserialize, Serialize};
use solana_sdk::{
    decode_error::DecodeError,
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signer::Signer,
    transaction::Transaction,
};
use thiserror::Error;

use crate::{
    sleipnir_authority, sleipnir_authority_id,
    sleipnir_processor::set_account_mod_data,
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
    ///  0.    `[WRITE, SIGNER]` Sleipnir Modify Authority
    ///  1..n. `[WRITE]` Accounts to modify
    ///  n+1.  `[SIGNER]` (Implicit NativeLoader)
    ModifyAccounts(HashMap<Pubkey, AccountModificationForInstruction>),
}

pub fn modify_accounts(
    keyed_account_mods: Vec<(Pubkey, AccountModification)>,
    recent_blockhash: Hash,
) -> Transaction {
    let ix = modify_accounts_instruction(keyed_account_mods);
    into_transaction(ix, recent_blockhash)
}

pub(crate) fn modify_accounts_instruction(
    keyed_account_mods: Vec<(Pubkey, AccountModification)>,
) -> Instruction {
    let mut account_metas =
        vec![AccountMeta::new(sleipnir_authority().pubkey(), true)];
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

fn into_transaction(
    instruction: Instruction,
    recent_blockhash: Hash,
) -> Transaction {
    let signers = &[&sleipnir_authority()];
    Transaction::new_signed_with_payer(
        &[instruction],
        Some(&sleipnir_authority_id()),
        signers,
        recent_blockhash,
    )
}
