use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use solana_sdk::{account::Account, pubkey::Pubkey};

use crate::args::MagicBaseIntentArgs;

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum MagicBlockInstruction {
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
    /// A second transaction [MagicBlockInstruction::AcceptScheduleCommits] has to run in order
    /// to finish scheduling the commit.
    ///
    /// # Account references
    /// - **0.**   `[WRITE, SIGNER]` Payer requesting the commit to be scheduled
    /// - **1.**   `[WRITE]`         Magic Context Account containing to which we store
    ///                              the scheduled commits
    /// - **2..n** `[]`              Accounts to be committed
    ScheduleCommit,

    /// This is the exact same instruction as [MagicBlockInstruction::ScheduleCommit] except
    /// that the [ScheduledCommit] is flagged such that when accounts are committed, a request
    /// to undelegate them is included with the same transaction.
    /// Additionally the validator will refuse anymore transactions for the specific account
    /// since they are no longer considered delegated to it.
    ///
    /// This is the first part of scheduling a commit.
    /// A second transaction [MagicBlockInstruction::AcceptScheduleCommits] has to run in order
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

    /// Records the attempt to realize a scheduled commit on chain.
    ///
    /// The signature of this transaction can be pre-calculated since we pass the
    /// ID of the scheduled commit and retrieve the signature from a globally
    /// stored hashmap.
    ///
    /// We implement it this way so we can log the signature of this transaction
    /// as part of the [MagicBlockInstruction::ScheduleCommit] instruction.
    ScheduledCommitSent(u64),
    ScheduleBaseIntent(MagicBaseIntentArgs),
}

// TODO: why that exists?
#[allow(unused)]
impl MagicBlockInstruction {
    pub(crate) fn index(&self) -> u8 {
        use MagicBlockInstruction::*;
        match self {
            ModifyAccounts(_) => 0,
            ScheduleCommit => 1,
            ScheduleCommitAndUndelegate => 2,
            AcceptScheduleCommits => 3,
            ScheduledCommitSent(_) => 4,
            ScheduleBaseIntent(_) => 5,
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
pub struct AccountModificationForInstruction {
    pub lamports: Option<u64>,
    pub owner: Option<Pubkey>,
    pub executable: Option<bool>,
    pub data_key: Option<u64>,
    pub rent_epoch: Option<u64>,
}
