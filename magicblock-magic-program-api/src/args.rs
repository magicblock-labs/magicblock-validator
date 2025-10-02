use serde::{Deserialize, Serialize};
use solana_program::{account_info::AccountInfo, instruction::AccountMeta};

use crate::Pubkey;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionArgs {
    pub escrow_index: u8,
    pub data: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct BaseActionArgs {
    pub args: ActionArgs,
    pub compute_units: u32, // compute units your action will use
    pub escrow_authority: u8, // index of account authorizing action on actor pda
    pub destination_program: Pubkey, // adress of destination program
    pub accounts: Vec<ShortAccountMeta>, // short account metas
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum CommitTypeArgs {
    Standalone(Vec<u8>), // indices on accounts
    WithBaseActions {
        committed_accounts: Vec<u8>, // indices of accounts
        base_actions: Vec<BaseActionArgs>,
    },
}

impl CommitTypeArgs {
    pub fn committed_accounts_indices(&self) -> &Vec<u8> {
        match self {
            Self::Standalone(value) => value,
            Self::WithBaseActions {
                committed_accounts, ..
            } => committed_accounts,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum UndelegateTypeArgs {
    Standalone,
    WithBaseActions { base_actions: Vec<BaseActionArgs> },
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct CommitAndUndelegateArgs {
    pub commit_type: CommitTypeArgs,
    pub undelegate_type: UndelegateTypeArgs,
}

impl CommitAndUndelegateArgs {
    pub fn committed_accounts_indices(&self) -> &Vec<u8> {
        self.commit_type.committed_accounts_indices()
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum MagicBaseIntentArgs {
    BaseActions(Vec<BaseActionArgs>),
    Commit(CommitTypeArgs),
    CommitAndUndelegate(CommitAndUndelegateArgs),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortAccountMeta {
    pub pubkey: Pubkey,
    pub is_writable: bool,
}

impl From<AccountMeta> for ShortAccountMeta {
    fn from(value: AccountMeta) -> Self {
        Self {
            pubkey: value.pubkey,
            is_writable: value.is_writable,
        }
    }
}

impl<'a> From<AccountInfo<'a>> for ShortAccountMeta {
    fn from(value: AccountInfo<'a>) -> Self {
        Self::from(&value)
    }
}

impl<'a> From<&AccountInfo<'a>> for ShortAccountMeta {
    fn from(value: &AccountInfo<'a>) -> Self {
        Self {
            pubkey: *value.key,
            is_writable: value.is_writable,
        }
    }
}
