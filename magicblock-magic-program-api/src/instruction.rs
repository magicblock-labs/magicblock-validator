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
}

impl MagicBlockInstruction {
    pub fn try_to_vec(&self) -> Result<Vec<u8>, bincode::error::EncodeError> {
        bincode::serde::encode_to_vec(self, bincode::config::legacy())
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

#[cfg(test)]
mod tests {
    use solana_program::instruction::{AccountMeta, Instruction};

    use super::*;
    use crate::args::{
        ActionArgs, BaseActionArgs, CommitAndUndelegateArgs, CommitTypeArgs,
        MagicBaseIntentArgs, MagicIntentBundleArgs, ScheduleTaskArgs,
        ShortAccountMeta, UndelegateTypeArgs,
    };

    /// Helper to verify bincode 2 (legacy config) produces identical bytes to bincode 1.3
    fn assert_bincode_compatible<T: Serialize>(value: &T, name: &str) {
        let bincode2_bytes =
            bincode::serde::encode_to_vec(value, bincode::config::legacy())
                .unwrap();
        let bincode1_bytes = bincode1::serialize(value).unwrap();
        assert_eq!(
            bincode2_bytes, bincode1_bytes,
            "{} serialization mismatch:\nbincode2: {:?}\nbincode1: {:?}",
            name, bincode2_bytes, bincode1_bytes
        );
    }

    /// Helper to verify bincode 2 can deserialize bincode 1 serialized data
    fn assert_bincode_deserialize_compatible<T>(value: &T, name: &str)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let bincode1_bytes = bincode1::serialize(value).unwrap();
        let bincode2_result: T = bincode::serde::decode_from_slice(
            &bincode1_bytes,
            bincode::config::legacy(),
        )
        .unwrap()
        .0;
        assert_eq!(&bincode2_result, value, "{} round-trip mismatch", name);
    }

    fn test_pubkey() -> Pubkey {
        Pubkey::new_from_array([1u8; 32])
    }

    fn test_pubkey2() -> Pubkey {
        Pubkey::new_from_array([2u8; 32])
    }

    #[test]
    fn test_noop_compatibility() {
        let instruction = MagicBlockInstruction::Noop(42);
        assert_bincode_compatible(&instruction, "Noop");
        assert_bincode_deserialize_compatible(&instruction, "Noop");
    }

    #[test]
    fn test_schedule_commit_compatibility() {
        let instruction = MagicBlockInstruction::ScheduleCommit;
        assert_bincode_compatible(&instruction, "ScheduleCommit");
        assert_bincode_deserialize_compatible(&instruction, "ScheduleCommit");
    }

    #[test]
    fn test_schedule_commit_and_undelegate_compatibility() {
        let instruction = MagicBlockInstruction::ScheduleCommitAndUndelegate;
        assert_bincode_compatible(&instruction, "ScheduleCommitAndUndelegate");
        assert_bincode_deserialize_compatible(
            &instruction,
            "ScheduleCommitAndUndelegate",
        );
    }

    #[test]
    fn test_accept_schedule_commits_compatibility() {
        let instruction = MagicBlockInstruction::AcceptScheduleCommits;
        assert_bincode_compatible(&instruction, "AcceptScheduleCommits");
        assert_bincode_deserialize_compatible(
            &instruction,
            "AcceptScheduleCommits",
        );
    }

    #[test]
    fn test_scheduled_commit_sent_compatibility() {
        let instruction =
            MagicBlockInstruction::ScheduledCommitSent((123, 456));
        assert_bincode_compatible(&instruction, "ScheduledCommitSent");
        assert_bincode_deserialize_compatible(
            &instruction,
            "ScheduledCommitSent",
        );
    }

    #[test]
    fn test_cancel_task_compatibility() {
        let instruction = MagicBlockInstruction::CancelTask { task_id: 999 };
        assert_bincode_compatible(&instruction, "CancelTask");
        assert_bincode_deserialize_compatible(&instruction, "CancelTask");
    }

    #[test]
    fn test_modify_accounts_compatibility() {
        let mut accounts = HashMap::new();
        accounts.insert(
            test_pubkey(),
            AccountModificationForInstruction {
                lamports: Some(1000),
                owner: Some(test_pubkey2()),
                executable: Some(false),
                data_key: Some(42),
                delegated: Some(true),
                confined: Some(false),
                remote_slot: Some(100),
            },
        );
        let instruction = MagicBlockInstruction::ModifyAccounts {
            accounts,
            message: Some("test message".to_string()),
        };
        assert_bincode_compatible(&instruction, "ModifyAccounts");
        assert_bincode_deserialize_compatible(&instruction, "ModifyAccounts");
    }

    #[test]
    fn test_schedule_base_intent_base_actions_compatibility() {
        let base_action = BaseActionArgs {
            args: ActionArgs::new(vec![1, 2, 3]),
            compute_units: 1000,
            escrow_authority: 0,
            destination_program: test_pubkey(),
            accounts: vec![ShortAccountMeta {
                pubkey: test_pubkey2(),
                is_writable: true,
            }],
        };
        let instruction = MagicBlockInstruction::ScheduleBaseIntent(
            MagicBaseIntentArgs::BaseActions(vec![base_action]),
        );
        assert_bincode_compatible(
            &instruction,
            "ScheduleBaseIntent::BaseActions",
        );
        assert_bincode_deserialize_compatible(
            &instruction,
            "ScheduleBaseIntent::BaseActions",
        );
    }

    #[test]
    fn test_schedule_base_intent_commit_standalone_compatibility() {
        let instruction = MagicBlockInstruction::ScheduleBaseIntent(
            MagicBaseIntentArgs::Commit(CommitTypeArgs::Standalone(vec![
                0, 1, 2,
            ])),
        );
        assert_bincode_compatible(
            &instruction,
            "ScheduleBaseIntent::Commit::Standalone",
        );
        assert_bincode_deserialize_compatible(
            &instruction,
            "ScheduleBaseIntent::Commit::Standalone",
        );
    }

    #[test]
    fn test_schedule_base_intent_commit_with_actions_compatibility() {
        let base_action = BaseActionArgs {
            args: ActionArgs::new(vec![4, 5, 6]).with_escrow_index(1),
            compute_units: 2000,
            escrow_authority: 2,
            destination_program: test_pubkey(),
            accounts: vec![],
        };
        let instruction = MagicBlockInstruction::ScheduleBaseIntent(
            MagicBaseIntentArgs::Commit(CommitTypeArgs::WithBaseActions {
                committed_accounts: vec![0, 1],
                base_actions: vec![base_action],
            }),
        );
        assert_bincode_compatible(
            &instruction,
            "ScheduleBaseIntent::Commit::WithBaseActions",
        );
        assert_bincode_deserialize_compatible(
            &instruction,
            "ScheduleBaseIntent::Commit::WithBaseActions",
        );
    }

    #[test]
    fn test_schedule_base_intent_commit_and_undelegate_compatibility() {
        let instruction = MagicBlockInstruction::ScheduleBaseIntent(
            MagicBaseIntentArgs::CommitAndUndelegate(CommitAndUndelegateArgs {
                commit_type: CommitTypeArgs::Standalone(vec![0]),
                undelegate_type: UndelegateTypeArgs::Standalone,
            }),
        );
        assert_bincode_compatible(
            &instruction,
            "ScheduleBaseIntent::CommitAndUndelegate",
        );
        assert_bincode_deserialize_compatible(
            &instruction,
            "ScheduleBaseIntent::CommitAndUndelegate",
        );
    }

    #[test]
    fn test_schedule_intent_bundle_compatibility() {
        let base_action = BaseActionArgs {
            args: ActionArgs::new(vec![7, 8, 9]),
            compute_units: 3000,
            escrow_authority: 1,
            destination_program: test_pubkey2(),
            accounts: vec![ShortAccountMeta {
                pubkey: test_pubkey(),
                is_writable: false,
            }],
        };
        let instruction = MagicBlockInstruction::ScheduleIntentBundle(
            MagicIntentBundleArgs {
                commit: Some(CommitTypeArgs::Standalone(vec![0, 1])),
                commit_and_undelegate: Some(CommitAndUndelegateArgs {
                    commit_type: CommitTypeArgs::Standalone(vec![2]),
                    undelegate_type: UndelegateTypeArgs::WithBaseActions {
                        base_actions: vec![base_action.clone()],
                    },
                }),
                standalone_actions: vec![base_action],
            },
        );
        assert_bincode_compatible(&instruction, "ScheduleIntentBundle");
        assert_bincode_deserialize_compatible(
            &instruction,
            "ScheduleIntentBundle",
        );
    }

    #[test]
    fn test_schedule_task_compatibility() {
        let instruction =
            MagicBlockInstruction::ScheduleTask(ScheduleTaskArgs {
                task_id: 12345,
                execution_interval_millis: 1000,
                iterations: 10,
                instructions: vec![Instruction {
                    program_id: test_pubkey(),
                    accounts: vec![AccountMeta::new(test_pubkey2(), true)],
                    data: vec![1, 2, 3, 4],
                }],
            });
        assert_bincode_compatible(&instruction, "ScheduleTask");
        assert_bincode_deserialize_compatible(&instruction, "ScheduleTask");
    }

    #[test]
    fn test_account_modification_compatibility() {
        let modification = AccountModification {
            pubkey: test_pubkey(),
            lamports: Some(5000),
            owner: Some(test_pubkey2()),
            executable: Some(true),
            data: Some(vec![0xde, 0xad, 0xbe, 0xef]),
            delegated: Some(true),
            confined: Some(false),
            remote_slot: Some(999),
        };
        assert_bincode_compatible(&modification, "AccountModification");
        assert_bincode_deserialize_compatible(
            &modification,
            "AccountModification",
        );
    }

    #[test]
    fn test_short_account_meta_compatibility() {
        let meta = ShortAccountMeta {
            pubkey: test_pubkey(),
            is_writable: true,
        };
        assert_bincode_compatible(&meta, "ShortAccountMeta");
        assert_bincode_deserialize_compatible(&meta, "ShortAccountMeta");
    }

    #[test]
    fn test_action_args_compatibility() {
        let args = ActionArgs::new(vec![10, 20, 30]).with_escrow_index(5);
        assert_bincode_compatible(&args, "ActionArgs");
        assert_bincode_deserialize_compatible(&args, "ActionArgs");
    }

    #[test]
    fn test_base_action_args_compatibility() {
        let args = BaseActionArgs {
            args: ActionArgs::new(vec![100, 200]),
            compute_units: 50000,
            escrow_authority: 3,
            destination_program: test_pubkey(),
            accounts: vec![
                ShortAccountMeta {
                    pubkey: test_pubkey(),
                    is_writable: true,
                },
                ShortAccountMeta {
                    pubkey: test_pubkey2(),
                    is_writable: false,
                },
            ],
        };
        assert_bincode_compatible(&args, "BaseActionArgs");
        assert_bincode_deserialize_compatible(&args, "BaseActionArgs");
    }

    #[test]
    fn test_magic_intent_bundle_args_compatibility() {
        let args = MagicIntentBundleArgs {
            commit: Some(CommitTypeArgs::Standalone(vec![0])),
            commit_and_undelegate: None,
            standalone_actions: vec![],
        };
        assert_bincode_compatible(&args, "MagicIntentBundleArgs");
        assert_bincode_deserialize_compatible(&args, "MagicIntentBundleArgs");
    }
}
