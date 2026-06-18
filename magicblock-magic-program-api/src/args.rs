use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    compat::{AccountInfo, AccountMeta, Instruction},
    Pubkey,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionArgs {
    pub escrow_index: u8,
    pub data: Vec<u8>,
}

impl ActionArgs {
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            escrow_index: 255,
            data,
        }
    }
    pub fn escrow_index(&self) -> u8 {
        self.escrow_index
    }

    pub fn data(&self) -> &Vec<u8> {
        &self.data
    }

    pub fn with_escrow_index(mut self, index: u8) -> Self {
        self.escrow_index = index;
        self
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct BaseActionArgs {
    pub args: ActionArgs,
    pub compute_units: u32, // compute units your action will use
    pub escrow_authority: u8, // index of account authorizing action on actor pda
    pub destination_program: Pubkey, // address of destination program
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
    CommitFinalize(CommitTypeArgs),
    CommitFinalizeAndUndelegate(CommitAndUndelegateArgs),
    CommitFinalizeCompressed(CommitTypeArgs),
    CommitFinalizeAndUndelegateCompressed(CommitAndUndelegateArgs),
}

#[derive(Clone, Default, Serialize, Debug, PartialEq, Eq)]
pub struct MagicIntentBundleArgs {
    pub commit: Option<CommitTypeArgs>,
    pub commit_and_undelegate: Option<CommitAndUndelegateArgs>,
    pub commit_finalize: Option<CommitTypeArgs>,
    pub commit_finalize_and_undelegate: Option<CommitAndUndelegateArgs>,
    pub standalone_actions: Vec<BaseActionArgs>,
    pub commit_finalize_compressed: Option<CommitTypeArgs>,
    pub commit_finalize_compressed_and_undelegate:
        Option<CommitAndUndelegateArgs>,
}

fn deserialize_trailing_option<'de, D, T>(
    deserializer: D,
) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).or(Ok(None))
}

impl<'de> Deserialize<'de> for MagicIntentBundleArgs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Workaround to handle deserializing older intent bundle args.
        #[derive(Deserialize)]
        struct MagicIntentBundleArgsWire {
            commit: Option<CommitTypeArgs>,
            commit_and_undelegate: Option<CommitAndUndelegateArgs>,
            commit_finalize: Option<CommitTypeArgs>,
            commit_finalize_and_undelegate: Option<CommitAndUndelegateArgs>,
            standalone_actions: Vec<BaseActionArgs>,
            #[serde(default, deserialize_with = "deserialize_trailing_option")]
            commit_finalize_compressed: Option<CommitTypeArgs>,
            #[serde(default, deserialize_with = "deserialize_trailing_option")]
            commit_finalize_compressed_and_undelegate:
                Option<CommitAndUndelegateArgs>,
        }

        let wire = MagicIntentBundleArgsWire::deserialize(deserializer)?;
        Ok(MagicIntentBundleArgs {
            commit: wire.commit,
            commit_and_undelegate: wire.commit_and_undelegate,
            commit_finalize: wire.commit_finalize,
            commit_finalize_and_undelegate: wire.commit_finalize_and_undelegate,
            standalone_actions: wire.standalone_actions,
            commit_finalize_compressed: wire.commit_finalize_compressed,
            commit_finalize_compressed_and_undelegate: wire
                .commit_finalize_compressed_and_undelegate,
        })
    }
}

impl From<MagicBaseIntentArgs> for MagicIntentBundleArgs {
    fn from(value: MagicBaseIntentArgs) -> Self {
        let mut this = Self::default();
        match value {
            MagicBaseIntentArgs::BaseActions(value) => {
                this.standalone_actions.extend(value)
            }
            MagicBaseIntentArgs::Commit(value) => this.commit = Some(value),
            MagicBaseIntentArgs::CommitAndUndelegate(value) => {
                this.commit_and_undelegate = Some(value)
            }
            MagicBaseIntentArgs::CommitFinalize(value) => {
                this.commit_finalize = Some(value)
            }
            MagicBaseIntentArgs::CommitFinalizeAndUndelegate(value) => {
                this.commit_finalize_and_undelegate = Some(value)
            }
            MagicBaseIntentArgs::CommitFinalizeCompressed(value) => {
                this.commit_finalize_compressed = Some(value)
            }
            MagicBaseIntentArgs::CommitFinalizeAndUndelegateCompressed(
                value,
            ) => this.commit_finalize_compressed_and_undelegate = Some(value),
        }

        this
    }
}

/// A compact account meta used for base-layer actions.
///
/// Unlike `solana_instruction::AccountMeta`, this type **does not** carry an
/// `is_signer` flag. Users cannot request signatures: the only signer available
/// is the validator.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortAccountMeta {
    pub pubkey: Pubkey,
    /// Whether this account should be marked **writable**
    /// in the Base layer instruction built from this action.
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

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct AddActionCallbackArgs {
    /// Flat index of the action to attach the callback to, ordered as:
    /// commit actions, commit_and_undelegate commit actions,
    /// commit_and_undelegate undelegate actions, standalone actions.
    pub action_index: u8,
    pub destination_program: Pubkey,
    pub discriminator: Vec<u8>,
    pub payload: Vec<u8>,
    pub compute_units: u32,
    pub accounts: Vec<ShortAccountMeta>,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct ScheduleTaskArgs {
    pub task_id: i64,
    pub execution_interval_millis: i64,
    pub iterations: i64,
    pub instructions: Vec<Instruction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskRequest {
    Schedule(ScheduleTaskRequest),
    Cancel(CancelTaskRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduleTaskRequest {
    /// Unique identifier for this task
    pub id: i64,
    /// Unsigned instructions to execute when triggered
    pub instructions: Vec<Instruction>,
    /// Authority that can modify or cancel this task
    pub authority: Pubkey,
    /// How frequently the task should be executed, in milliseconds
    pub execution_interval_millis: i64,
    /// Number of times this task will be executed
    pub iterations: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CancelTaskRequest {
    /// Unique identifier for the task to cancel
    pub task_id: i64,
    /// Authority that can cancel this task
    pub authority: Pubkey,
}

impl TaskRequest {
    pub fn id(&self) -> i64 {
        match self {
            Self::Schedule(request) => request.id,
            Self::Cancel(request) => request.task_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use legacy_magicblock_magic_program_api::args::{
        ActionArgs as LegacyActionArgs, BaseActionArgs as LegacyBaseActionArgs,
        CommitAndUndelegateArgs as LegacyCommitAndUndelegateArgs,
        CommitTypeArgs as LegacyCommitTypeArgs,
        MagicIntentBundleArgs as LegacyMagicIntentBundleArgs,
        ShortAccountMeta as LegacyShortAccountMeta,
        UndelegateTypeArgs as LegacyUndelegateTypeArgs,
    };

    use super::*;

    fn to_legacy_base_action_args(
        args: BaseActionArgs,
    ) -> LegacyBaseActionArgs {
        LegacyBaseActionArgs {
            args: LegacyActionArgs {
                escrow_index: args.args.escrow_index,
                data: args.args.data,
            },
            compute_units: args.compute_units,
            escrow_authority: args.escrow_authority,
            destination_program: args.destination_program,
            accounts: args
                .accounts
                .into_iter()
                .map(|meta| LegacyShortAccountMeta {
                    pubkey: meta.pubkey,
                    is_writable: meta.is_writable,
                })
                .collect(),
        }
    }

    fn from_legacy_base_action_args(
        args: LegacyBaseActionArgs,
    ) -> BaseActionArgs {
        BaseActionArgs {
            args: ActionArgs {
                escrow_index: args.args.escrow_index,
                data: args.args.data,
            },
            compute_units: args.compute_units,
            escrow_authority: args.escrow_authority,
            destination_program: args.destination_program,
            accounts: args
                .accounts
                .into_iter()
                .map(|meta| ShortAccountMeta {
                    pubkey: meta.pubkey,
                    is_writable: meta.is_writable,
                })
                .collect(),
        }
    }

    fn from_legacy_commit_type_args(
        args: LegacyCommitTypeArgs,
    ) -> CommitTypeArgs {
        match args {
            LegacyCommitTypeArgs::Standalone(accounts) => {
                CommitTypeArgs::Standalone(accounts.clone())
            }
            LegacyCommitTypeArgs::WithBaseActions {
                committed_accounts,
                base_actions,
            } => CommitTypeArgs::WithBaseActions {
                committed_accounts: committed_accounts.clone(),
                base_actions: base_actions
                    .into_iter()
                    .map(from_legacy_base_action_args)
                    .collect(),
            },
        }
    }

    fn from_legacy_undelegate_type_args(
        args: LegacyUndelegateTypeArgs,
    ) -> UndelegateTypeArgs {
        match args {
            LegacyUndelegateTypeArgs::Standalone => {
                UndelegateTypeArgs::Standalone
            }
            LegacyUndelegateTypeArgs::WithBaseActions { base_actions } => {
                UndelegateTypeArgs::WithBaseActions {
                    base_actions: base_actions
                        .into_iter()
                        .map(from_legacy_base_action_args)
                        .collect(),
                }
            }
        }
    }

    fn from_legacy_commit_and_undelegate_args(
        args: LegacyCommitAndUndelegateArgs,
    ) -> CommitAndUndelegateArgs {
        CommitAndUndelegateArgs {
            commit_type: from_legacy_commit_type_args(args.commit_type),
            undelegate_type: from_legacy_undelegate_type_args(
                args.undelegate_type,
            ),
        }
    }

    #[test]
    fn magic_intent_bundle_args_appends_compressed_fields_after_standalone_actions(
    ) {
        let standalone = vec![BaseActionArgs {
            args: ActionArgs {
                escrow_index: 0,
                data: vec![0xFF, 0xFE],
            },
            compute_units: 300_000,
            escrow_authority: 7,
            destination_program: Pubkey::new_from_array([0x99; 32]),
            accounts: vec![ShortAccountMeta {
                pubkey: Pubkey::new_from_array([0x88; 32]),
                is_writable: true,
            }],
        }];

        let legacy = LegacyMagicIntentBundleArgs {
            commit: Some(LegacyCommitTypeArgs::Standalone(vec![2, 3])),
            commit_and_undelegate: None,
            commit_finalize: None,
            commit_finalize_and_undelegate: None,
            standalone_actions: standalone
                .clone()
                .into_iter()
                .map(to_legacy_base_action_args)
                .collect(),
        };
        let legacy_bytes = bincode::serialize(&legacy).unwrap();

        let current = MagicIntentBundleArgs {
            commit: legacy.commit.clone().map(from_legacy_commit_type_args),
            commit_and_undelegate: legacy
                .commit_and_undelegate
                .clone()
                .map(from_legacy_commit_and_undelegate_args),
            commit_finalize: legacy
                .commit_finalize
                .clone()
                .map(from_legacy_commit_type_args),
            commit_finalize_and_undelegate: legacy
                .commit_finalize_and_undelegate
                .clone()
                .map(from_legacy_commit_and_undelegate_args),
            standalone_actions: legacy
                .standalone_actions
                .clone()
                .into_iter()
                .map(from_legacy_base_action_args)
                .collect(),
            commit_finalize_compressed: None,
            commit_finalize_compressed_and_undelegate: None,
        };
        let current_bytes = bincode::serialize(&current).unwrap();

        assert_eq!(current_bytes.len(), legacy_bytes.len() + 2);
        assert_eq!(&current_bytes[..legacy_bytes.len()], &legacy_bytes[..]);
        assert_eq!(&current_bytes[legacy_bytes.len()..], &[0, 0]);

        let decoded: MagicIntentBundleArgs =
            bincode::deserialize(&legacy_bytes).unwrap();
        assert_eq!(decoded, current);
    }
}
