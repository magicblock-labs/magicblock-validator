use std::{cell::RefCell, collections::HashSet};

use magicblock_core::magic_program::args::{
    ActionArgs, BaseActionArgs, CommitAndUndelegateArgs, CommitTypeArgs,
    MagicBaseIntentArgs, UndelegateTypeArgs,
};
use serde::{Deserialize, Serialize};
use solana_log_collector::ic_msg;
use solana_program_runtime::{
    __private::{Hash, InstructionError, ReadableAccount, TransactionContext},
    invoke_context::InvokeContext,
};
use solana_sdk::{
    account::{Account, AccountSharedData},
    clock::Slot,
    pubkey::Pubkey,
    transaction::Transaction,
};

use crate::{
    instruction_utils::InstructionUtils,
    utils::accounts::{
        get_instruction_account_short_meta_with_idx,
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
    },
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
pub struct ScheduledBaseIntent {
    pub id: u64,
    pub slot: Slot,
    pub blockhash: Hash,
    pub action_sent_transaction: Transaction,
    pub payer: Pubkey,
    // Scheduled action
    pub base_intent: MagicBaseIntent,
}

impl ScheduledBaseIntent {
    pub fn try_new(
        args: &MagicBaseIntentArgs,
        commit_id: u64,
        slot: Slot,
        payer_pubkey: &Pubkey,
        context: &ConstructionContext<'_, '_>,
    ) -> Result<ScheduledBaseIntent, InstructionError> {
        let action = MagicBaseIntent::try_from_args(args, context)?;

        let blockhash = context.invoke_context.environment_config.blockhash;
        let action_sent_transaction =
            InstructionUtils::scheduled_commit_sent(commit_id, blockhash);
        Ok(ScheduledBaseIntent {
            id: commit_id,
            slot,
            blockhash,
            payer: *payer_pubkey,
            action_sent_transaction,
            base_intent: action,
        })
    }

    pub fn get_committed_accounts(&self) -> Option<&Vec<CommittedAccountV2>> {
        self.base_intent.get_committed_accounts()
    }

    pub fn get_committed_accounts_mut(
        &mut self,
    ) -> Option<&mut Vec<CommittedAccountV2>> {
        self.base_intent.get_committed_accounts_mut()
    }

    pub fn get_committed_pubkeys(&self) -> Option<Vec<Pubkey>> {
        self.base_intent.get_committed_pubkeys()
    }

    pub fn is_undelegate(&self) -> bool {
        self.base_intent.is_undelegate()
    }

    pub fn is_empty(&self) -> bool {
        self.base_intent.is_empty()
    }
}

// BaseIntent user wants to send to base layer
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MagicBaseIntent {
    /// Actions without commitment or undelegation
    BaseActions(Vec<BaseAction>),
    Commit(CommitType),
    CommitAndUndelegate(CommitAndUndelegate),
}

impl MagicBaseIntent {
    pub fn try_from_args(
        args: &MagicBaseIntentArgs,
        context: &ConstructionContext<'_, '_>,
    ) -> Result<MagicBaseIntent, InstructionError> {
        match args {
            MagicBaseIntentArgs::BaseActions(base_actions) => {
                let base_actions = base_actions
                    .iter()
                    .map(|args| BaseAction::try_from_args(args, context))
                    .collect::<Result<Vec<BaseAction>, InstructionError>>()?;
                Ok(MagicBaseIntent::BaseActions(base_actions))
            }
            MagicBaseIntentArgs::Commit(type_) => {
                let commit = CommitType::try_from_args(type_, context)?;
                Ok(MagicBaseIntent::Commit(commit))
            }
            MagicBaseIntentArgs::CommitAndUndelegate(type_) => {
                let commit_and_undelegate =
                    CommitAndUndelegate::try_from_args(type_, context)?;
                Ok(MagicBaseIntent::CommitAndUndelegate(commit_and_undelegate))
            }
        }
    }

    pub fn is_undelegate(&self) -> bool {
        match &self {
            MagicBaseIntent::BaseActions(_) => false,
            MagicBaseIntent::Commit(_) => false,
            MagicBaseIntent::CommitAndUndelegate(_) => true,
        }
    }

    pub fn get_committed_accounts(&self) -> Option<&Vec<CommittedAccountV2>> {
        match self {
            MagicBaseIntent::BaseActions(_) => None,
            MagicBaseIntent::Commit(t) => Some(t.get_committed_accounts()),
            MagicBaseIntent::CommitAndUndelegate(t) => {
                Some(t.get_committed_accounts())
            }
        }
    }

    pub fn get_committed_accounts_mut(
        &mut self,
    ) -> Option<&mut Vec<CommittedAccountV2>> {
        match self {
            MagicBaseIntent::BaseActions(_) => None,
            MagicBaseIntent::Commit(t) => Some(t.get_committed_accounts_mut()),
            MagicBaseIntent::CommitAndUndelegate(t) => {
                Some(t.get_committed_accounts_mut())
            }
        }
    }

    pub fn get_committed_pubkeys(&self) -> Option<Vec<Pubkey>> {
        self.get_committed_accounts().map(|accounts| {
            accounts.iter().map(|account| account.pubkey).collect()
        })
    }

    pub fn is_empty(&self) -> bool {
        match self {
            MagicBaseIntent::BaseActions(actions) => actions.is_empty(),
            MagicBaseIntent::Commit(t) => t.is_empty(),
            MagicBaseIntent::CommitAndUndelegate(t) => t.is_empty(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitAndUndelegate {
    pub commit_action: CommitType,
    pub undelegate_action: UndelegateType,
}

impl CommitAndUndelegate {
    pub fn try_from_args(
        args: &CommitAndUndelegateArgs,
        context: &ConstructionContext<'_, '_>,
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

    pub fn get_committed_accounts(&self) -> &Vec<CommittedAccountV2> {
        self.commit_action.get_committed_accounts()
    }

    pub fn get_committed_accounts_mut(
        &mut self,
    ) -> &mut Vec<CommittedAccountV2> {
        self.commit_action.get_committed_accounts_mut()
    }

    pub fn is_empty(&self) -> bool {
        self.commit_action.is_empty()
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
pub struct BaseAction {
    pub compute_units: u32,
    pub destination_program: Pubkey,
    pub escrow_authority: Pubkey,
    pub data_per_program: ProgramArgs,
    pub account_metas_per_program: Vec<ShortAccountMeta>,
}

impl BaseAction {
    pub fn try_from_args(
        args: &BaseActionArgs,
        context: &ConstructionContext<'_, '_>,
    ) -> Result<BaseAction, InstructionError> {
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
                    "BaseAction: destination_program must be an executable. got: {}",
                    destination_program_pubkey
                )
            );
            return Err(InstructionError::AccountNotExecutable);
        }

        // Since action on Base layer performed on behalf of some escrow
        // We need to ensure that action was authorized by legit owner
        let authority_pubkey = get_instruction_pubkey_with_idx(
            context.transaction_context,
            args.escrow_authority as u16,
        )?;
        if !context.signers.contains(authority_pubkey) {
            ic_msg!(
                context.invoke_context,
                &format!(
                    "BaseAction: authority pubkey must sign transaction: {}",
                    authority_pubkey
                )
            );

            return Err(InstructionError::MissingRequiredSignature);
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

        Ok(BaseAction {
            compute_units: args.compute_units,
            destination_program: destination_program_pubkey,
            escrow_authority: *authority_pubkey,
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
    WithBaseActions {
        committed_accounts: Vec<CommittedAccountV2>,
        base_actions: Vec<BaseAction>,
    },
}

impl CommitType {
    // TODO: move to processor
    fn validate_accounts(
        accounts: &[CommittedAccountRef],
        context: &ConstructionContext<'_, '_>,
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

    pub fn try_from_args(
        args: &CommitTypeArgs,
        context: &ConstructionContext<'_, '_>,
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
            CommitTypeArgs::WithBaseActions {
                committed_accounts,
                base_actions,
            } => {
                let committed_accounts_ref = Self::extract_commit_accounts(
                    committed_accounts,
                    context.transaction_context,
                )?;
                Self::validate_accounts(&committed_accounts_ref, context)?;

                let base_actions = base_actions
                    .iter()
                    .map(|args| BaseAction::try_from_args(args, context))
                    .collect::<Result<Vec<BaseAction>, InstructionError>>()?;
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

                Ok(CommitType::WithBaseActions {
                    committed_accounts,
                    base_actions,
                })
            }
        }
    }

    pub fn get_committed_accounts(&self) -> &Vec<CommittedAccountV2> {
        match self {
            Self::Standalone(committed_accounts) => committed_accounts,
            Self::WithBaseActions {
                committed_accounts, ..
            } => committed_accounts,
        }
    }

    pub fn get_committed_accounts_mut(
        &mut self,
    ) -> &mut Vec<CommittedAccountV2> {
        match self {
            Self::Standalone(committed_accounts) => committed_accounts,
            Self::WithBaseActions {
                committed_accounts, ..
            } => committed_accounts,
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::Standalone(committed_accounts) => {
                committed_accounts.is_empty()
            }
            Self::WithBaseActions {
                committed_accounts, ..
            } => committed_accounts.is_empty(),
        }
    }
}

/// No CommitedAccounts since it is only used with CommitAction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UndelegateType {
    Standalone,
    WithBaseActions(Vec<BaseAction>),
}

impl UndelegateType {
    pub fn try_from_args(
        args: &UndelegateTypeArgs,
        context: &ConstructionContext<'_, '_>,
    ) -> Result<UndelegateType, InstructionError> {
        match args {
            UndelegateTypeArgs::Standalone => Ok(UndelegateType::Standalone),
            UndelegateTypeArgs::WithBaseActions { base_actions } => {
                let base_actions = base_actions
                    .iter()
                    .map(|base_action| {
                        BaseAction::try_from_args(base_action, context)
                    })
                    .collect::<Result<Vec<BaseAction>, InstructionError>>()?;
                Ok(UndelegateType::WithBaseActions(base_actions))
            }
        }
    }
}
