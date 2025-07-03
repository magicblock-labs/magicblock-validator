use std::{cell::RefCell, collections::HashSet};

use serde::{Deserialize, Serialize};
use solana_log_collector::ic_msg;
use solana_program_runtime::{
    __private::{Hash, InstructionError, ReadableAccount, TransactionContext},
    invoke_context::InvokeContext,
};
use solana_sdk::{
    account::{Account, AccountSharedData},
    clock::Slot,
    transaction::Transaction,
};

use crate::{
    args::{
        ActionArgs, CommitAndUndelegateArgs, CommitTypeArgs, L1ActionArgs,
        MagicL1MessageArgs, UndelegateTypeArgs,
    },
    instruction_utils::InstructionUtils,
    utils::accounts::{
        get_instruction_account_short_meta_with_idx,
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
    },
    Pubkey,
};

/// Context necessary for construction of Schedule Action
pub struct ConstructionContext<'a, 'ic> {
    parent_program_id: Option<Pubkey>,
    signers: &'a HashSet<Pubkey>,
    pub transaction_context: &'a TransactionContext,
    pub invoke_context: &'a mut InvokeContext<'ic>,
}

impl<'a, 'ic> ConstructionContext<'a, 'ic> {
    pub fn new(
        parent_program_id: Option<Pubkey>,
        signers: &'a HashSet<Pubkey>,
        transaction_context: &'a TransactionContext,
        invoke_context: &'a mut InvokeContext<'ic>,
    ) -> Self {
        Self {
            parent_program_id,
            signers,
            transaction_context,
            invoke_context,
        }
    }
}

/// Scheduled action to be executed on base layer
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledL1Message {
    pub id: u64,
    pub slot: Slot,
    pub blockhash: Hash,
    pub action_sent_transaction: Transaction,
    pub payer: Pubkey,
    // Scheduled action
    pub l1_message: MagicL1Message,
}

impl ScheduledL1Message {
    pub fn try_new<'a>(
        args: &MagicL1MessageArgs,
        commit_id: u64,
        slot: Slot,
        payer_pubkey: &Pubkey,
        context: &ConstructionContext<'a, '_>,
    ) -> Result<ScheduledL1Message, InstructionError> {
        let action = MagicL1Message::try_from_args(args, &context)?;

        let blockhash = context.invoke_context.environment_config.blockhash;
        let action_sent_transaction =
            InstructionUtils::scheduled_commit_sent(commit_id, blockhash);
        Ok(ScheduledL1Message {
            id: commit_id,
            slot,
            blockhash,
            payer: *payer_pubkey,
            action_sent_transaction,
            l1_message: action,
        })
    }
}

// L1Message user wants to send to base layer
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MagicL1Message {
    /// Actions without commitment or undelegation
    L1Actions(Vec<L1Action>),
    Commit(CommitType),
    CommitAndUndelegate(CommitAndUndelegate),
}

impl MagicL1Message {
    pub fn try_from_args<'a>(
        args: &MagicL1MessageArgs,
        context: &ConstructionContext<'a, '_>,
    ) -> Result<MagicL1Message, InstructionError> {
        match args {
            MagicL1MessageArgs::L1Actions(l1_actions) => {
                let l1_actions = l1_actions
                    .iter()
                    .map(|args| L1Action::try_from_args(args, context))
                    .collect::<Result<Vec<L1Action>, InstructionError>>()?;
                Ok(MagicL1Message::L1Actions(l1_actions))
            }
            MagicL1MessageArgs::Commit(type_) => {
                let commit = CommitType::try_from_args(type_, context)?;
                Ok(MagicL1Message::Commit(commit))
            }
            MagicL1MessageArgs::CommitAndUndelegate(type_) => {
                let commit_and_undelegate =
                    CommitAndUndelegate::try_from_args(type_, context)?;
                Ok(MagicL1Message::CommitAndUndelegate(commit_and_undelegate))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitAndUndelegate {
    pub commit_action: CommitType,
    pub undelegate_action: UndelegateType,
}

impl CommitAndUndelegate {
    pub fn try_from_args<'a>(
        args: &CommitAndUndelegateArgs,
        context: &ConstructionContext<'a, '_>,
    ) -> Result<CommitAndUndelegate, InstructionError> {
        let commit_action =
            CommitType::try_from_args(&args.commit_type, context)?;
        let undelegate_action =
            UndelegateType::try_from_args(&args.undelegate_type, context)?;

        Ok(Self {
            commit_action,
            undelegate_action,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramArgs {
    pub escrow_index: u8,
    pub data: Vec<u8>,
}

impl From<ActionArgs> for ProgramArgs {
    fn from(value: ActionArgs) -> Self {
        Self {
            escrow_index: value.escrow_index,
            data: value.data,
        }
    }
}

impl From<&ActionArgs> for ProgramArgs {
    fn from(value: &ActionArgs) -> Self {
        value.clone().into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortAccountMeta {
    pub pubkey: Pubkey,
    pub is_writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct L1Action {
    pub destination_program: Pubkey,
    pub data_per_program: ProgramArgs,
    pub account_metas_per_program: Vec<ShortAccountMeta>,
}

impl L1Action {
    pub fn try_from_args<'a>(
        args: &L1ActionArgs,
        context: &ConstructionContext<'a, '_>,
    ) -> Result<L1Action, InstructionError> {
        let destination_program_pubkey = *get_instruction_pubkey_with_idx(
            context.transaction_context,
            args.destination_program as u16,
        )?;
        let destination_program = get_instruction_account_with_idx(
            context.transaction_context,
            args.destination_program as u16,
        )?;

        if !destination_program.borrow().executable() {
            ic_msg!(
                context.invoke_context,
                &format!(
                    "L1Action: destination_program must be an executable. got: {}",
                    destination_program_pubkey
                )
            );
            return Err(InstructionError::AccountNotExecutable);
        }

        let account_metas = args
            .accounts
            .iter()
            .map(|i| {
                get_instruction_account_short_meta_with_idx(
                    context.transaction_context,
                    *i as u16,
                )
            })
            .collect::<Result<Vec<ShortAccountMeta>, InstructionError>>()?;

        Ok(L1Action {
            destination_program: destination_program_pubkey,
            data_per_program: args.args.clone().into(),
            account_metas_per_program: account_metas,
        })
    }
}

type CommittedAccountRef<'a> = (Pubkey, &'a RefCell<AccountSharedData>);
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedAccountV2 {
    pub pubkey: Pubkey,
    pub account: Account,
}

impl<'a> From<CommittedAccountRef<'a>> for CommittedAccountV2 {
    fn from(value: CommittedAccountRef<'a>) -> Self {
        Self {
            pubkey: value.0,
            account: value.1.borrow().to_owned().into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommitType {
    /// Regular commit without actions
    /// TODO: feels like ShortMeta isn't needed
    Standalone(Vec<CommittedAccountV2>), // accounts to commit
    /// Commits accounts and runs actions
    WithL1Actions {
        committed_accounts: Vec<CommittedAccountV2>,
        l1_actions: Vec<L1Action>,
    },
}

impl CommitType {
    // TODO: move to processor
    fn validate_accounts<'a>(
        accounts: &[CommittedAccountRef],
        context: &ConstructionContext<'a, '_>,
    ) -> Result<(), InstructionError> {
        accounts.iter().try_for_each(|(pubkey, account)| {
            let owner = *account.borrow().owner();
            if context.parent_program_id != Some(owner) && !context.signers.contains(pubkey) {
                match context.parent_program_id {
                    None => {
                        ic_msg!(
                            context.invoke_context,
                            "ScheduleCommit ERR: failed to find parent program id"
                        );
                        Err(InstructionError::InvalidInstructionData)
                    }
                    Some(parent_id) => {
                        ic_msg!(
                            context.invoke_context,
                            "ScheduleCommit ERR: account {} must be owned by {} or be a signer, but is owned by {}",
                            pubkey, parent_id, owner
                        );
                        Err(InstructionError::InvalidAccountOwner)
                    }
                }
            } else {
                Ok(())
            }
        })
    }

    // I delegated an account, now the owner is delegation program
    // parent_program_id != Some(&acc_owner) should fail. or any modification on ER
    // ER perceives owner as old one, hence for ER those are valid txs
    // On commit_and_undelegate and commit we will set owner to DLP, for latter temparerily
    // The owner shall be real owner on chain
    // So first:
    // 1. Validate
    // 2. Fetch current account states
    // TODO: 3. switch the ownership
    pub fn extract_commit_accounts<'a>(
        account_indices: &[u8],
        transaction_context: &'a TransactionContext,
    ) -> Result<Vec<CommittedAccountRef<'a>>, InstructionError> {
        account_indices
            .iter()
            .map(|i| {
                let account = get_instruction_account_with_idx(
                    transaction_context,
                    *i as u16,
                )?;
                let pubkey = *get_instruction_pubkey_with_idx(
                    transaction_context,
                    *i as u16,
                )?;

                Ok((pubkey, account))
            })
            .collect::<Result<_, InstructionError>>()
    }

    pub fn try_from_args<'a>(
        args: &CommitTypeArgs,
        context: &ConstructionContext<'a, '_>,
    ) -> Result<CommitType, InstructionError> {
        match args {
            CommitTypeArgs::Standalone(accounts) => {
                let committed_accounts_ref = Self::extract_commit_accounts(
                    accounts,
                    context.transaction_context,
                )?;
                Self::validate_accounts(&committed_accounts_ref, context)?;
                let committed_accounts = committed_accounts_ref
                    .into_iter()
                    .map(|el| {
                        let mut committed_account: CommittedAccountV2 =
                            el.into();
                        committed_account.account.owner = context
                            .parent_program_id
                            .unwrap_or(committed_account.account.owner);

                        committed_account
                    })
                    .collect();

                Ok(CommitType::Standalone(committed_accounts))
            }
            CommitTypeArgs::WithL1Actions {
                committed_accounts,
                l1_actions,
            } => {
                let committed_accounts_ref = Self::extract_commit_accounts(
                    committed_accounts,
                    context.transaction_context,
                )?;
                Self::validate_accounts(&committed_accounts_ref, context)?;

                let l1_actions = l1_actions
                    .iter()
                    .map(|args| L1Action::try_from_args(args, context))
                    .collect::<Result<Vec<L1Action>, InstructionError>>()?;
                let committed_accounts = committed_accounts_ref
                    .into_iter()
                    .map(|el| {
                        let mut committed_account: CommittedAccountV2 =
                            el.into();
                        committed_account.account.owner = context
                            .parent_program_id
                            .unwrap_or(committed_account.account.owner);

                        committed_account
                    })
                    .collect();

                Ok(CommitType::WithL1Actions {
                    committed_accounts,
                    l1_actions,
                })
            }
        }
    }
}

/// No CommitedAccounts since it is only used with CommitAction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UndelegateType {
    Standalone,
    WithL1Actions(Vec<L1Action>),
}

impl UndelegateType {
    pub fn try_from_args<'a>(
        args: &UndelegateTypeArgs,
        context: &ConstructionContext<'a, '_>,
    ) -> Result<UndelegateType, InstructionError> {
        match args {
            UndelegateTypeArgs::Standalone => Ok(UndelegateType::Standalone),
            UndelegateTypeArgs::WithL1Actions { l1_actions } => {
                let l1_actions = l1_actions
                    .iter()
                    .map(|l1_actions| {
                        L1Action::try_from_args(l1_actions, context)
                    })
                    .collect::<Result<Vec<L1Action>, InstructionError>>()?;
                Ok(UndelegateType::WithL1Actions(l1_actions))
            }
        }
    }
}
