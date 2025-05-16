use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use solana_log_collector::ic_msg;
use solana_program_runtime::{
    __private::{Hash, InstructionError, ReadableAccount, TransactionContext},
    invoke_context::InvokeContext,
};
use solana_sdk::{clock::Slot, transaction::Transaction};

use crate::{
    args::{
        CallHandlerArgs, CommitAndUndelegateArgs, CommitTypeArgs, HandlerArgs,
        MagicActionArgs, UndelegateTypeArgs,
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
    transaction_context: &'a TransactionContext,
    invoke_context: &'a mut InvokeContext<'ic>,
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
pub struct ScheduleAction {
    pub id: u64,
    pub slot: Slot,
    pub blockhash: Hash,
    pub commit_sent_transaction: Transaction,
    pub payer: Pubkey,
    // Scheduled action
    pub action: MagicAction,
}

impl ScheduleAction {
    pub fn try_new<'a>(
        args: &MagicActionArgs,
        commit_id: u64,
        slot: Slot,
        payer_pubkey: &Pubkey,
        context: &ConstructionContext<'a, '_>,
    ) -> Result<ScheduleAction, InstructionError> {
        let action = MagicAction::try_from_args(args, &context)?;

        let blockhash = context.invoke_context.environment_config.blockhash;
        let commit_sent_transaction =
            InstructionUtils::scheduled_commit_sent(commit_id, blockhash);
        let commit_sent_sig = commit_sent_transaction.signatures[0];

        Ok(ScheduleAction {
            id: commit_id,
            slot,
            blockhash,
            payer: *payer_pubkey,
            commit_sent_transaction,
            action,
        })
    }
}

// Action that user wants to perform on base layer
pub enum MagicAction {
    /// Actions without commitment or undelegation
    CallHandler(Vec<CallHandler>),
    Commit(CommitType),
    CommitAndUndelegate(CommitAndUndelegate),
}

impl MagicAction {
    pub fn try_from_args<'a>(
        args: &MagicActionArgs,
        context: &ConstructionContext<'a, '_>,
    ) -> Result<MagicAction, InstructionError> {
        match args {
            MagicActionArgs::L1Action(call_handlers_args) => {
                let call_handlers = call_handlers_args
                    .iter()
                    .map(|args| CallHandler::try_from_args(args, context))
                    .collect::<Result<Vec<CallHandler>, InstructionError>>()?;
                Ok(MagicAction::CallHandler(call_handlers))
            }
            MagicActionArgs::Commit(type_) => {
                let commit = CommitType::try_from_args(type_, context)?;
                Ok(MagicAction::Commit(commit))
            }
            MagicActionArgs::CommitAndUndelegate(type_) => {
                let commit_and_undelegate =
                    CommitAndUndelegate::try_from_args(type_, context)?;
                Ok(MagicAction::CommitAndUndelegate(commit_and_undelegate))
            }
        }
    }
}

impl CommitType {
    fn validate_accounts<'a>(
        account_indices: &[u8],
        context: &ConstructionContext<'a, '_>,
    ) -> Result<(), InstructionError> {
        account_indices.iter().try_for_each(|index| {
            let acc_pubkey = get_instruction_pubkey_with_idx(context.transaction_context, *index as u16)?;
            let acc = get_instruction_account_with_idx(context.transaction_context, *index as u16)?;
            let acc_owner = *acc.borrow().owner();

            if context.parent_program_id.as_ref() != Some(acc_pubkey) && !context.signers.contains(acc_pubkey) {
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
                            acc_pubkey, parent_id, acc_owner
                        );
                        Err(InstructionError::InvalidAccountOwner)
                    }
                }
            } else {
                Ok(())
            }
        })
    }

    fn extract_commit_accounts<'a>(
        account_indices: &[u8],
        context: &ConstructionContext<'a, '_>,
    ) -> Result<Vec<CommittedAccountV2>, InstructionError> {
        account_indices
            .iter()
            .map(|i| {
                let account = get_instruction_account_with_idx(
                    context.transaction_context,
                    *i as u16,
                )?;
                let owner = *account.borrow().owner();
                let short_meta = get_instruction_account_short_meta_with_idx(
                    context.transaction_context,
                    *i as u16,
                )?;

                Ok(CommittedAccountV2 {
                    short_meta,
                    owner: context.parent_program_id.unwrap_or(owner),
                })
            })
            .collect::<Result<Vec<CommittedAccountV2>, InstructionError>>()
    }

    pub fn try_from_args<'a>(
        args: &CommitTypeArgs,
        context: &ConstructionContext<'a, '_>,
    ) -> Result<CommitType, InstructionError> {
        match args {
            CommitTypeArgs::Standalone(accounts) => {
                Self::validate_accounts(accounts, context)?;
                let committed_accounts =
                    Self::extract_commit_accounts(accounts, context)?;

                Ok(CommitType::Standalone(committed_accounts))
            }
            CommitTypeArgs::WithHandler {
                committed_accounts,
                call_handlers,
            } => {
                Self::validate_accounts(committed_accounts, context)?;
                let committed_accounts =
                    Self::extract_commit_accounts(committed_accounts, context)?;
                let call_handlers = call_handlers
                    .iter()
                    .map(|args| CallHandler::try_from_args(args, context))
                    .collect::<Result<Vec<CallHandler>, InstructionError>>()?;

                Ok(CommitType::WithHandler {
                    committed_accounts,
                    call_handlers,
                })
            }
        }
    }
}

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
pub struct Handler {
    pub escrow_index: u8,
    pub data: Vec<u8>,
}

impl From<HandlerArgs> for Handler {
    fn from(value: HandlerArgs) -> Self {
        Self {
            escrow_index: value.escrow_index,
            data: value.data,
        }
    }
}

impl From<&HandlerArgs> for Handler {
    fn from(value: &HandlerArgs) -> Self {
        value.clone().into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortAccountMeta {
    pub pubkey: Pubkey,
    pub is_writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallHandler {
    pub destination_program: Pubkey,
    pub data_per_program: Handler,
    pub account_metas_per_program: Vec<ShortAccountMeta>,
}

impl CallHandler {
    pub fn try_from_args<'a>(
        args: &CallHandlerArgs,
        context: &ConstructionContext<'a, '_>,
    ) -> Result<CallHandler, InstructionError> {
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
                    "CallHandler: destination_program must be an executable. got: {}",
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

        Ok(CallHandler {
            destination_program: destination_program_pubkey,
            data_per_program: args.args.clone().into(),
            account_metas_per_program: account_metas,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedAccountV2 {
    pub short_meta: ShortAccountMeta,
    // TODO(GabrielePicco): We should read the owner from the delegation record rather
    // than deriving/storing it. To remove once the cloning pipeline allow us to easily access the owner.
    pub owner: Pubkey,
}

pub enum CommitType {
    /// Regular commit without actions
    Standalone(Vec<CommittedAccountV2>), // accounts to commit
    /// Commits accounts and runs actions
    WithHandler {
        committed_accounts: Vec<CommittedAccountV2>,
        call_handlers: Vec<CallHandler>,
    },
}

/// No CommitedAccounts since it is only used with CommitAction.
pub enum UndelegateType {
    Standalone,
    WithHandler(Vec<CallHandler>),
}

impl UndelegateType {
    pub fn try_from_args<'a>(
        args: &UndelegateTypeArgs,
        context: &ConstructionContext<'a, '_>,
    ) -> Result<UndelegateType, InstructionError> {
        match args {
            UndelegateTypeArgs::Standalone => Ok(UndelegateType::Standalone),
            UndelegateTypeArgs::WithHandler { call_handlers } => {
                let call_handlers = call_handlers
                    .iter()
                    .map(|call_handler| {
                        CallHandler::try_from_args(call_handler, context)
                    })
                    .collect::<Result<Vec<CallHandler>, InstructionError>>()?;
                Ok(UndelegateType::WithHandler(call_handlers))
            }
        }
    }
}
