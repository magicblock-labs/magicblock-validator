use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use solana_program::pubkey::Pubkey;

use crate::args::{MagicBaseIntentArgs, ScheduleTaskArgs};

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

    /// Schedules the compressed accounts provided at end of accounts Vec to be committed.
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
    ScheduleCompressedCommit,

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

    /// This is the exact same instruction as [MagicBlockInstruction::ScheduleCompressedCommit] except
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
    ///                               the scheduled commits
    /// - **2..n** `[]`              Accounts to be committed and undelegated
    ScheduleCompressedCommitAndUndelegate,

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
    /// Args: (intent_id, bump) - bump is needed in order to guarantee unique transactions
    ScheduledCommitSent((u64, u64)),
    ScheduleBaseIntent(MagicBaseIntentArgs),

    /// Schedule a new task for execution
    ///
    /// # Account references
    /// - **0.**    `[WRITE, SIGNER]` Payer (payer)
    /// - **1.**    `[WRITE]`         Task context account
    /// - **2..n**  `[]`              Accounts included in the task
    ScheduleTask(ScheduleTaskArgs),

    /// Cancel a task
    ///
    /// # Account references
    /// - **0.** `[WRITE, SIGNER]` Task authority
    /// - **1.** `[WRITE]`         Task context account
    CancelTask {
        task_id: i64,
    },

    /// Disables the executable check, needed to modify the data of a program
    /// in preparation to deploying it via LoaderV4 and to modify its authority.
    ///
    /// # Account references
    /// - **0.** `[SIGNER]`         Validator authority
    DisableExecutableCheck,

    /// Enables the executable check, and should run after
    /// a program is deployed with the LoaderV4 and we modified its authority
    ///
    /// # Account references
    /// - **0.** `[SIGNER]`         Validator authority
    EnableExecutableCheck,
}

impl MagicBlockInstruction {
    pub fn try_to_vec(&self) -> Result<Vec<u8>, bincode::Error> {
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
    // TODO(bmuddha/thlorenz): deprecate rent_epoch
    // https://github.com/magicblock-labs/magicblock-validator/issues/580
    pub rent_epoch: Option<u64>,
    pub delegated: Option<bool>,
    pub compressed: Option<bool>,
    pub confined: Option<bool>,
}

#[derive(Default, Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct AccountModificationForInstruction {
    pub lamports: Option<u64>,
    pub owner: Option<Pubkey>,
    pub executable: Option<bool>,
    pub data_key: Option<u64>,
    // TODO(bmuddha/thlorenz): deprecate rent_epoch
    // https://github.com/magicblock-labs/magicblock-validator/issues/580
    pub rent_epoch: Option<u64>,
    pub delegated: Option<bool>,
    pub compressed: Option<bool>,
    pub confined: Option<bool>,
}
