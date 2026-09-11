use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{
    args::{
        AddActionCallbackArgs, MagicBaseIntentArgs, MagicIntentBundleArgs,
        ScheduleTaskArgs,
    },
    compat::Instruction,
    Pubkey,
};

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum MagicBlockInstruction {
    /// Modify one or more accounts
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Validator Authority. Authorizes the modification. | WRITE, SIGNER |
    /// | `1..n` | Modified accounts. Accounts to modify. | WRITE |
    /// | `n+1` | NativeLoader. Implicit NativeLoader account. | SIGNER |
    ModifyAccounts {
        accounts: HashMap<Pubkey, AccountModificationForInstruction>,
        message: Option<String>,
    },

    /// Schedules the accounts provided at end of accounts Vec to be committed
    /// and finalized in a single DLP instruction.
    /// It should be invoked from the program whose PDA accounts are to be
    /// committed.
    ///
    /// This is the first part of scheduling a commit.
    /// A second transaction [MagicBlockInstruction::AcceptScheduleCommits] has to run in order
    /// to finish scheduling the commit.
    ///
    /// Layout: `{ payer, magic_context, [magic_fee_vault], committee_0, ... }`
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Payer. Requests the commit to be scheduled. | WRITE, SIGNER |
    /// | `1` | Magic Context. Stores scheduled commits. | WRITE |
    /// | `2` | Magic fee-vault. Required for delegated, non-confined payers; otherwise optional. If present when not required, it is validated as usual but skipped for fee charging. | WRITE, OPTIONAL |
    /// | `m..n` | Commit accounts. `m` is `2` when the fee-vault is omitted and `3` when present. | - |
    ScheduleCommit,

    /// This is the exact same instruction as [MagicBlockInstruction::ScheduleCommit] except
    /// that the scheduled intent is flagged such that when accounts are committed and finalized,
    /// a request to undelegate them is included with the same transaction.
    /// Additionally the validator will refuse anymore transactions for the specific account
    /// since they are no longer considered delegated to it.
    ///
    /// This is the first part of scheduling a commit.
    /// A second transaction [MagicBlockInstruction::AcceptScheduleCommits] has to run in order
    /// to finish scheduling the commit.
    ///
    /// Layout: `{ payer, magic_context, [magic_fee_vault], committee_0, ... }`
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Payer. Requests the commit to be scheduled. | WRITE, SIGNER |
    /// | `1` | Magic Context. Stores scheduled commits. | WRITE |
    /// | `2` | Magic fee-vault. Required for delegated, non-confined payers; otherwise optional. If present when not required, it is validated as usual but skipped for fee charging. | WRITE, OPTIONAL |
    /// | `m..n` | Commit and undelegate accounts. `m` is `2` when the fee-vault is omitted and `3` when present. | - |
    ScheduleCommitAndUndelegate,

    /// Moves the scheduled commit from the MagicContext to the global scheduled commits
    /// map. This is the second part of scheduling a commit.
    ///
    /// It is run at the start of the slot to update the global scheduled commits map just
    /// in time for the validator to realize the commits right after.
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Validator Authority. Authorizes accepting scheduled commits. | SIGNER |
    /// | `1` | Magic Context. Contains the initially scheduled commits. | WRITE |
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
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | MagicBlock program. Must match the MagicBlock program ID. | - |
    /// | `1` | Validator Authority. Must match the validator identity. | SIGNER |
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
    /// Layout: `{ payer, magic_context, [magic_fee_vault], account_0, ... }`
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Payer. Requests the intent to be scheduled. | WRITE, SIGNER |
    /// | `1` | Magic Context. Stores scheduled intents. | WRITE |
    /// | `2` | Magic fee-vault. Required for delegated, non-confined payers; otherwise optional. If present when not required, it is validated as usual but skipped for fee charging. | WRITE, OPTIONAL |
    /// | `m..n` | Intent accounts. Accounts referenced by the intent, including action accounts. `m` is `2` when the fee-vault is omitted and `3` when present. | - |
    ///
    /// # Data
    /// The embedded [`MagicBaseIntentArgs`] encodes account references by indices into the
    /// accounts array.
    ScheduleBaseIntent(MagicBaseIntentArgs),

    /// Schedule a new task for execution
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Payer. Requests task scheduling. | WRITE, SIGNER |
    ScheduleTask(ScheduleTaskArgs),

    /// Cancel a task
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Task authority. Requests task cancellation. | WRITE, SIGNER |
    CancelTask { task_id: i64 },

    /// Disables the executable check, needed to modify the data of a program
    /// in preparation to deploying it via LoaderV4 and to modify its authority.
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Validator Authority. Authorizes the executable-check change. | SIGNER |
    DisableExecutableCheck,

    /// Enables the executable check, and should run after
    /// a program is deployed with the LoaderV4 and we modified its authority
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Validator Authority. Authorizes the executable-check change. | SIGNER |
    EnableExecutableCheck,

    /// Noop instruction
    Noop(u64),

    /// Schedules execution of a *bundle* of intents in a single instruction.
    ///
    /// An intent bundle is an atomic unit of work executed by the validator on the Base layer,
    /// such as:
    /// - standalone base actions
    /// - an optional `Commit`
    /// - an optional `CommitAndUndelegate`
    ///
    /// This is the recommended scheduling path when the caller wants to submit multiple
    /// independent intents while paying account overhead only once.
    ///
    /// Layout: `{ payer, magic_context, [magic_fee_vault], account_0, ... }`
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Payer. Requests the bundle to be scheduled. | WRITE, SIGNER |
    /// | `1` | Magic Context. Stores scheduled intents. | WRITE |
    /// | `2` | Magic fee-vault. Required for delegated, non-confined payers; otherwise optional. If present when not required, it is validated as usual but skipped for fee charging. | WRITE, OPTIONAL |
    /// | `m..n` | Intent accounts. All accounts referenced by any intent. `m` is `2` when the fee-vault is omitted and `3` when present. | - |
    ///
    /// # Data
    /// The embedded [`MagicIntentBundleArgs`] encodes account references by their actual
    /// indices in the accounts array.
    ScheduleIntentBundle(MagicIntentBundleArgs),

    /// Creates a new ephemeral account with rent paid by a sponsor.
    /// The account is automatically owned by the calling program (CPI caller).
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Sponsor. Pays rent; can be PDA or oncurve. | WRITE |
    /// | `1` | Ephemeral account. Account to create; must have 0 lamports. | WRITE |
    /// | `2` | Vault. Receives rent payment. | WRITE |
    CreateEphemeralAccount {
        /// Initial data length in bytes
        data_len: u32,
    },

    /// Resizes an existing ephemeral account, adjusting rent accordingly.
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Sponsor. Pays or receives the rent difference. | WRITE |
    /// | `1` | Ephemeral account. Account to resize. | WRITE |
    /// | `2` | Vault. Holds lamports for rent transfer. | WRITE |
    ResizeEphemeralAccount {
        /// New data length in bytes
        new_data_len: u32,
    },

    /// Closes an ephemeral account, refunding rent to the sponsor.
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Sponsor. Receives rent refund. | WRITE |
    /// | `1` | Ephemeral account. Account to close. | WRITE |
    /// | `2` | Vault. Source of rent refund. | WRITE |
    CloseEphemeralAccount,

    /// Unsed instruction slot.
    /// -- can be repurposed --
    /// This variant was originally used for `ScheduleCommitFinalize`, but that
    /// instruction was removed. It is intentionally left unused so the wire
    /// discriminant can be repurposed in a future protocol update.
    Unused,

    /// Clone a single account that fits in one transaction (<63KB data).
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Validator Authority. Authorizes cloning. | WRITE, SIGNER |
    /// | `1` | Account. Account to clone. | WRITE |
    CloneAccount {
        pubkey: Pubkey,
        data: Vec<u8>,
        fields: AccountCloneFields,
        actions: Vec<Instruction>,
    },

    /// Initialize a multi-transaction clone for a large account.
    /// Adds the pubkey to PENDING_CLONES. Must be followed by CloneAccountContinue
    /// with is_last=true to complete.
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Validator Authority. Authorizes cloning. | WRITE, SIGNER |
    /// | `1` | Account. Account to clone. | WRITE |
    CloneAccountInit {
        pubkey: Pubkey,
        total_data_len: u32,
        initial_data: Vec<u8>,
        fields: AccountCloneFields,
    },

    /// Continue a multi-transaction clone with the next data chunk.
    /// If is_last=true, removes the pubkey from PENDING_CLONES.
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Validator Authority. Authorizes cloning. | WRITE, SIGNER |
    /// | `1` | Account. Account being cloned. | WRITE |
    CloneAccountContinue {
        pubkey: Pubkey,
        offset: u32,
        data: Vec<u8>,
        is_last: bool,
        actions: Vec<Instruction>,
        needs_undelegation: bool,
    },

    /// Cleanup a partial clone on failure. Removes from PENDING_CLONES
    /// and deletes the account.
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Validator Authority. Authorizes cleanup. | WRITE, SIGNER |
    /// | `1` | Account. Account to clean up. | WRITE |
    CleanupPartialClone { pubkey: Pubkey },

    /// Finalize program deployment from a buffer account.
    /// Does the following:
    /// 1. Copies data from buffer account to program account
    /// 2. Sets loader header with Retracted status and validator authority
    /// 3. Closes buffer account
    ///
    /// After this, LoaderV4::Deploy must be called, then SetProgramAuthority.
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Validator Authority. Authorizes finalization. | SIGNER |
    /// | `1` | Program account. Program account to finalize. | WRITE |
    /// | `2` | Buffer account. Closed after finalization. | WRITE |
    FinalizeProgramFromBuffer { remote_slot: u64 },

    /// Finalize V1 program deployment from a buffer account.
    /// V1 programs are converted to V3 (upgradeable loader) format.
    /// Does the following:
    /// 1. Creates program_data account with V3 ProgramData header + ELF
    /// 2. Creates program account with V3 Program header
    /// 3. Closes buffer account
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Validator Authority. Authorizes finalization. | SIGNER |
    /// | `1` | Program account. Program account to finalize. | WRITE |
    /// | `2` | Program data account. Created with V3 ProgramData header and ELF data. | WRITE |
    /// | `3` | Buffer account. Closed after finalization. | WRITE |
    FinalizeV1ProgramFromBuffer { remote_slot: u64, authority: Pubkey },

    /// Update the authority in a LoaderV4 program header.
    /// Used after Deploy to set the final chain authority.
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Validator Authority. Authorizes the authority update. | SIGNER |
    /// | `1` | Program account. LoaderV4 program account to update. | WRITE |
    SetProgramAuthority { authority: Pubkey },

    /// Attaches a callback to a previously scheduled action in the latest intent.
    ///
    /// Must be called via CPI from the program that originally scheduled the
    /// action. The caller's program ID is checked against the action's
    /// `source_program` field for authorization.
    ///
    /// A callback fee is deducted from the delegated payer into the magic fee-vault.
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Payer. Delegated payer charged for the callback. | WRITE, SIGNER |
    /// | `1` | Magic Context. Contains the latest scheduled intent. | WRITE |
    /// | `2` | Magic fee-vault. Required; receives the callback fee. | WRITE |
    AddActionCallback(AddActionCallbackArgs),

    /// Evict an account from the ephemeral validator.
    /// Sets the account to empty state (lamports=0, data=[], owner=default,
    /// delegated=false, confined=false, ephemeral=true).
    /// The ephemeral+default-owner combination triggers automatic removal
    /// from AccountsDb during commit (see AccountsDb::upsert).
    /// Rejects accounts that are delegated or undelegating.
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Validator Authority. Authorizes eviction. | SIGNER |
    /// | `1` | Account. Account to evict. | WRITE |
    EvictAccount { pubkey: Pubkey },

    /// Executes a crank
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Validator Authority. Authorizes crank execution. | SIGNER |
    /// | `1` | Crank signer PDA. PDA signer used by embedded instructions. | - |
    /// | `2..n` | Instruction accounts. Accounts required by the embedded instructions. | - |
    ExecuteCrank {
        authority: Pubkey,
        instructions: Vec<Instruction>,
    },
}

impl MagicBlockInstruction {
    pub fn try_to_vec(&self) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(self)
    }
}

#[derive(Default, Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct AccountModification {
    pub pubkey: Pubkey,
    pub owner: Option<Pubkey>,
    pub delegated: Option<bool>,
    pub confined: Option<bool>,
}

#[derive(Default, Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct AccountModificationForInstruction {
    pub owner: Option<Pubkey>,
    pub delegated: Option<bool>,
    pub confined: Option<bool>,
}

/// Common fields for cloning an account.
#[derive(
    Default, Clone, Copy, Serialize, Deserialize, Debug, PartialEq, Eq,
)]
pub struct AccountCloneFields {
    pub lamports: u64,
    pub owner: Pubkey,
    pub executable: bool,
    pub delegated: bool,
    pub confined: bool,
    pub remote_slot: u64,
}

/// Instruction(s) for Callback Executor builtin-program
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum CallbackInstruction {
    /// Executes a callback
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Validator authority | SIGNER |
    /// | `1` | Callback signer PDA | - |
    /// | `2..n` | Accounts required by the embedded instructions | - |
    ExecuteCallback { instruction: Instruction },
}

/// Instruction(s) for the post-delegation action executor builtin-program.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum PostDelegationActionExecutorInstruction {
    /// Executes post-delegation actions immediately after a matching delegated
    /// clone instruction in the same transaction.
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Validator authority | SIGNER |
    /// | `1` | Delegated clone target | - |
    /// | `2` | Instructions sysvar | - |
    /// | `3..n` | Accounts required by the embedded instructions | - |
    Execute {
        cloned_account_pubkey: Pubkey,
        actions: Vec<Instruction>,
    },

    /// Schedules undelegation immediately after a matching delegated clone
    /// instruction in the same transaction.
    ///
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Validator authority | SIGNER |
    /// | `1` | Delegated clone target | - |
    /// | `2` | Instructions sysvar | - |
    /// | `3` | Magic Context account | WRITE |
    ScheduleUndelegation { cloned_account_pubkey: Pubkey },
}

/// Instruction(s) for the ephemeral system builtin-program: creates,
/// resizes, and closes ephemeral accounts.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum EphemeralSystemInstruction {
    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Sponsor account (pays rent, can be PDA or oncurve) | WRITE |
    /// | `1` | Ephemeral account to create (must have 0 lamports) | WRITE |
    /// | `2` | Vault account (receives rent payment) | WRITE |
    CreateEphemeralAccount { data_len: u32 },

    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Sponsor account (pays/receives rent difference) | WRITE |
    /// | `1` | Ephemeral account to resize | WRITE |
    /// | `2` | Vault account (holds/receives lamports for rent transfer) | WRITE |
    ResizeEphemeralAccount { new_data_len: u32 },

    /// # Account references
    /// | Index | Account | Access |
    /// | --- | --- | --- |
    /// | `0` | Sponsor account (receives rent refund) | WRITE |
    /// | `1` | Ephemeral account to close | WRITE |
    /// | `2` | Vault account (source of rent refund) | WRITE |
    CloseEphemeralAccount,
}
