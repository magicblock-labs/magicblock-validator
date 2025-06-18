use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionArgs {
    pub escrow_index: u8,
    pub data: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct L1ActionArgs {
    pub args: ActionArgs,
    pub destination_program: u8, // index of the account
    pub accounts: Vec<u8>,       // indices of account
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum CommitTypeArgs {
    Standalone(Vec<u8>), // indices on accounts
    WithL1Actions {
        committed_accounts: Vec<u8>, // indices of accounts
        l1_actions: Vec<L1ActionArgs>,
    },
}

impl CommitTypeArgs {
    pub fn committed_accounts_indices(&self) -> &Vec<u8> {
        match self {
            Self::Standalone(value) => value,
            Self::WithL1Actions {
                committed_accounts, ..
            } => committed_accounts,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum UndelegateTypeArgs {
    Standalone,
    WithL1Actions { l1_actions: Vec<L1ActionArgs> },
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
pub enum MagicL1MessageArgs {
    L1Actions(Vec<L1ActionArgs>),
    Commit(CommitTypeArgs),
    CommitAndUndelegate(CommitAndUndelegateArgs),
}
