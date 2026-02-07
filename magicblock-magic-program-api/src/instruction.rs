use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use solana_program::pubkey::Pubkey;

use crate::args::{
    MagicBaseIntentArgs, MagicIntentBundleArgs, ScheduleTaskArgs,
};

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum MagicBlockInstruction {
    /// Modify one or more accounts
    ///
    /// # Account references
    ///  - **0.**    `[WRITE, SIGNER]` Validator Authority
    ///  - **1..n.** `[WRITE]` Accounts to modify
    ///  - **n+1**  `[SIGNER]` (Implicit NativeLoader)
    ModifyAccounts {
        accounts: HashMap<Pubkey, AccountModificationForInstruction>,
        message: Option<String>,
    },

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
    ///   the scheduled commits
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
    ///   the scheduled commits
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
    /// Args: (intent_id, bump) - bump is needed in order to guarantee unique transactions
    ScheduledCommitSent((u64, u64)),

    /// Schedules execution of a single *base intent*.
    ///
    /// A "base intent" is an atomic unit of work executed by the validator on the Base layer,
    /// such as:
    /// - executing standalone base actions (`BaseActions`)
    /// - committing a set of accounts (`Commit`)
    /// - committing and undelegating accounts, optionally with post-actions (`CommitAndUndelegate`)
    ///
    /// This instruction is the legacy/single-intent variant of scheduling. For batching multiple
    /// independent intents into a single instruction, see [`MagicBlockInstruction::ScheduleIntentBundle`].
    ///
    /// # Account references
    /// - **0.**   `[WRITE, SIGNER]` Payer requesting the intent to be scheduled
    /// - **1.**   `[WRITE]`         Magic Context account
    /// - **2..n** `[]`              Accounts referenced by the intent (including action accounts)
    ///
    /// # Data
    /// The embedded [`MagicBaseIntentArgs`] encodes account references by indices into the
    /// accounts array (compact representation).
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
    CancelTask { task_id: i64 },

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

    /// Noop instruction
    Noop(u64),

    /// Schedules execution of a *bundle* of intents in a single instruction.
    ///
    /// A "intent bundle" is an atomic unit of work executed by the validator on the Base layer,
    /// such as:
    /// - standalone base actions
    /// - an optional `Commit`
    /// - an optional `CommitAndUndelegate`
    ///
    /// This is the recommended scheduling path when the caller wants to submit multiple
    /// independent intents while paying account overhead only once.
    ///
    /// # Account references
    /// - **0.**   `[WRITE, SIGNER]` Payer requesting the bundle to be scheduled
    /// - **1.**   `[WRITE]`         Magic Context account
    /// - **2..n** `[]`              All accounts referenced by any intent in the bundle
    ///
    /// # Data
    /// The embedded [`MagicIntentBundleArgs`] encodes account references by indices into the
    /// accounts array.
    ScheduleIntentBundle(MagicIntentBundleArgs),

    /// Creates a new ephemeral account with rent paid by a sponsor.
    /// The account is automatically owned by the calling program (CPI caller).
    ///
    /// # Account references
    /// - **0.** `[WRITE]` Sponsor account (pays rent, can be PDA or oncurve)
    /// - **1.** `[WRITE]` Ephemeral account to create (must have 0 lamports)
    /// - **2.** `[WRITE]` Vault account (receives rent payment)
    CreateEphemeralAccount {
        /// Initial data length in bytes
        data_len: u32,
    },

    /// Resizes an existing ephemeral account, adjusting rent accordingly.
    ///
    /// # Account references
    /// - **0.** `[WRITE]` Sponsor account (pays/receives rent difference)
    /// - **1.** `[WRITE]` Ephemeral account to resize
    /// - **2.** `[WRITE]` Vault account (holds/receives lamports for rent transfer)
    ResizeEphemeralAccount {
        /// New data length in bytes
        new_data_len: u32,
    },

    /// Closes an ephemeral account, refunding rent to the sponsor.
    ///
    /// # Account references
    /// - **0.** `[WRITE]` Sponsor account (receives rent refund)
    /// - **1.** `[WRITE]` Ephemeral account to close
    /// - **2.** `[WRITE]` Vault account (source of rent refund)
    CloseEphemeralAccount,
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
    pub delegated: Option<bool>,
    pub confined: Option<bool>,
    pub remote_slot: Option<u64>,
}

#[derive(Default, Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct AccountModificationForInstruction {
    pub lamports: Option<u64>,
    pub owner: Option<Pubkey>,
    pub executable: Option<bool>,
    pub data_key: Option<u64>,
    pub delegated: Option<bool>,
    pub confined: Option<bool>,
    pub remote_slot: Option<u64>,
}
