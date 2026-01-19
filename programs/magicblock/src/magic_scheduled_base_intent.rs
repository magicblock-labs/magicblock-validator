use std::{cell::RefCell, collections::HashSet};

use magicblock_core::{
    token_programs::{
        try_remap_ata_to_eata, EATA_PROGRAM_ID, TOKEN_PROGRAM_ID,
    },
    Slot,
};
use magicblock_magic_program_api::args::{
    ActionArgs, BaseActionArgs, CommitAndUndelegateArgs, CommitTypeArgs,
    MagicBaseIntentArgs, MagicIntentBundleArgs, ShortAccountMeta,
    UndelegateTypeArgs,
};
use serde::{Deserialize, Serialize};
use solana_account::{Account, AccountSharedData, ReadableAccount};
use solana_hash::Hash;
use solana_log_collector::ic_msg;
use solana_program_runtime::{
    __private::{InstructionError, TransactionContext},
    invoke_context::InvokeContext,
};
use solana_pubkey::Pubkey;
use solana_transaction::Transaction;

use crate::{
    instruction_utils::InstructionUtils,
    utils::accounts::{
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
        get_writable_with_idx,
    },
    validator::validator_authority_id,
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
pub struct ScheduledIntentBundle {
    pub id: u64,
    pub slot: Slot,
    pub blockhash: Hash,
    pub intent_bundle_sent_transaction: Transaction,
    pub payer: Pubkey,
    /// Scheduled intent bundle
    // TODO(edwin): rename
    pub intent_bundle: MagicIntentBundle,
}

impl ScheduledIntentBundle {
    pub fn try_new(
        args: MagicIntentBundleArgs,
        commit_id: u64,
        slot: Slot,
        payer_pubkey: &Pubkey,
        context: &ConstructionContext<'_, '_>,
    ) -> Result<ScheduledIntentBundle, InstructionError> {
        let intent_bundle = MagicIntentBundle::try_from_args(args, context)?;
        let blockhash = context.invoke_context.environment_config.blockhash;
        let intent_bundle_sent_transaction =
            InstructionUtils::scheduled_commit_sent(commit_id, blockhash);

        Ok(ScheduledIntentBundle {
            id: commit_id,
            slot,
            blockhash,
            payer: *payer_pubkey,
            intent_bundle_sent_transaction,
            intent_bundle,
        })
    }

    pub fn get_undelegated_accounts(&self) -> Option<&Vec<CommittedAccount>> {
        Some(
            self.intent_bundle
                .commit_and_undelegate
                .as_ref()?
                .get_committed_accounts(),
        )
    }

    pub fn get_committed_pubkeys(&self) -> Option<Vec<Pubkey>> {
        self.intent_bundle.get_committed_pubkeys()
    }

    pub fn is_undelegate(&self) -> bool {
        self.intent_bundle.is_undelegate()
    }

    pub fn is_empty(&self) -> bool {
        self.intent_bundle.is_empty()
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

// Bundle of BaseIntents
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MagicIntentBundle {
    pub commit: Option<CommitType>,
    pub commit_and_undelegate: Option<CommitAndUndelegate>,
    pub standalone_actions: Vec<BaseAction>,
}

impl From<MagicBaseIntent> for MagicIntentBundle {
    fn from(value: MagicBaseIntent) -> Self {
        let mut this = Self::default();
        match value {
            MagicBaseIntent::BaseActions(value) => {
                this.standalone_actions.extend(value)
            }
            MagicBaseIntent::Commit(value) => this.commit = Some(value),
            MagicBaseIntent::CommitAndUndelegate(value) => {
                this.commit_and_undelegate = Some(value)
            }
        }

        this
    }
}

impl MagicIntentBundle {
    pub fn try_from_args(
        args: MagicIntentBundleArgs,
        context: &ConstructionContext<'_, '_>,
    ) -> Result<Self, InstructionError> {
        Self::validate(&args)?;

        let commit = args
            .commit
            .map(|value| CommitType::try_from_args(value, context))
            .transpose()?;
        let commit_and_undelegate = args
            .commit_and_undelegate
            .map(|value| CommitAndUndelegate::try_from_args(value, context))
            .transpose()?;
        let actions = args
            .standalone_actions
            .into_iter()
            .map(|args| BaseAction::try_from_args(args, context))
            .collect::<Result<Vec<BaseAction>, InstructionError>>()?;

        Ok(Self {
            commit,
            commit_and_undelegate,
            standalone_actions: actions,
        })
    }

    /// Cross intent validation:
    /// 1. Set of committed accounts shall not overlap with
    /// set of undelegated accounts
    /// 2. None for now :)
    fn validate(args: &MagicIntentBundleArgs) -> Result<(), InstructionError> {
        let committed_set: Option<HashSet<_>> =
            args.commit.as_ref().map(|el| {
                el.committed_accounts_indices().iter().copied().collect()
            });
        let Some(committed_set) = committed_set else {
            return Ok(());
        };

        args.commit_and_undelegate
            .as_ref()
            .and_then(|el| {
                let has_cross_reference = el
                    .committed_accounts_indices()
                    .iter()
                    .any(|ind| committed_set.contains(ind));
                if has_cross_reference {
                    Some(Ok(()))
                } else {
                    // TODO(edwin): add msg here?
                    Some(Err(InstructionError::InvalidInstructionData))
                }
            })
            .unwrap_or(Ok(()))
    }

    pub fn is_undelegate(&self) -> bool {
        self.commit_and_undelegate.is_some()
    }

    /// Returns all the accounts that will be committed,
    /// including the ones that will be undelegated as well
    pub fn get_committed_accounts(&self) -> Option<Vec<CommittedAccount>> {
        let committed = self
            .commit
            .as_ref()
            .map(|el| el.get_committed_accounts().to_owned());

        let undelegated = self
            .commit_and_undelegate
            .as_ref()
            .map(|el| el.get_committed_accounts().to_owned());

        match (committed, undelegated) {
            (None, None) => None,
            (Some(mut a), Some(b)) => {
                a.extend(b);
                Some(a)
            }
            (Some(a), None) | (None, Some(a)) => Some(a),
        }
    }

    pub fn get_committed_pubkeys(&self) -> Option<Vec<Pubkey>> {
        self.get_committed_accounts().map(|accounts| {
            accounts.iter().map(|account| account.pubkey).collect()
        })
    }

    pub fn is_empty(&self) -> bool {
        let has_committed = self
            .commit
            .as_ref()
            .map(|el| !el.is_empty())
            .unwrap_or(false);
        let has_committed_and_undelegated = self
            .commit_and_undelegate
            .as_ref()
            .map(|el| !el.is_empty())
            .unwrap_or(false);
        let has_actions = !self.standalone_actions.is_empty();

        has_committed || has_committed_and_undelegated || has_actions
    }
}

impl MagicBaseIntent {
    pub fn try_from_args(
        args: MagicBaseIntentArgs,
        context: &ConstructionContext<'_, '_>,
    ) -> Result<MagicBaseIntent, InstructionError> {
        match args {
            MagicBaseIntentArgs::BaseActions(base_actions) => {
                let base_actions = base_actions
                    .into_iter()
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

    pub fn get_committed_accounts(&self) -> Option<&Vec<CommittedAccount>> {
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
    ) -> Option<&mut Vec<CommittedAccount>> {
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
        args: CommitAndUndelegateArgs,
        context: &ConstructionContext<'_, '_>,
    ) -> Result<CommitAndUndelegate, InstructionError> {
        let account_indices = args.commit_type.committed_accounts_indices();
        Self::validate(account_indices.as_slice(), context)?;

        let commit_action =
            CommitType::try_from_args(args.commit_type, context)?;
        let undelegate_action =
            UndelegateType::try_from_args(args.undelegate_type, context)?;

        Ok(Self {
            commit_action,
            undelegate_action,
        })
    }

    pub fn validate(
        account_indices: &[u8],
        context: &ConstructionContext<'_, '_>,
    ) -> Result<(), InstructionError> {
        account_indices.iter().copied().try_for_each(|idx| {
            let is_writable = get_writable_with_idx(context.transaction_context, idx as u16)?;
            let delegated = get_instruction_account_with_idx(context.transaction_context, idx as u16)?;
            if is_writable && delegated.borrow().delegated() {
                Ok(())
            } else {
                let pubkey = get_instruction_pubkey_with_idx(context.transaction_context, idx as u16)?;
                ic_msg!(
                    context.invoke_context,
                    "ScheduleCommit ERR: account {} is required to be writable and delegated in order to be undelegated",
                    pubkey
                );
                Err(InstructionError::ReadonlyDataModified)
            }
        })
    }

    pub fn get_committed_accounts(&self) -> &Vec<CommittedAccount> {
        self.commit_action.get_committed_accounts()
    }

    pub fn get_committed_accounts_mut(&mut self) -> &mut Vec<CommittedAccount> {
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
pub struct BaseAction {
    pub compute_units: u32,
    pub destination_program: Pubkey,
    pub escrow_authority: Pubkey,
    pub data_per_program: ProgramArgs,
    pub account_metas_per_program: Vec<ShortAccountMeta>,
}

impl BaseAction {
    pub fn try_from_args(
        args: BaseActionArgs,
        context: &ConstructionContext<'_, '_>,
    ) -> Result<BaseAction, InstructionError> {
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

        Ok(BaseAction {
            compute_units: args.compute_units,
            destination_program: args.destination_program,
            escrow_authority: *authority_pubkey,
            data_per_program: args.args.into(),
            account_metas_per_program: args.accounts,
        })
    }
}

type CommittedAccountRef<'a> = (Pubkey, &'a RefCell<AccountSharedData>);
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedAccount {
    pub pubkey: Pubkey,
    pub account: Account,
    pub remote_slot: u64,
}

impl<'a> From<CommittedAccountRef<'a>> for CommittedAccount {
    fn from(value: CommittedAccountRef<'a>) -> Self {
        let account = value.1.borrow();
        let remote_slot = account.remote_slot();
        Self {
            pubkey: value.0,
            account: account.to_owned().into(),
            remote_slot,
        }
    }
}

impl CommittedAccount {
    /// Build a CommittedAccount from an AccountSharedData reference, optionally
    /// overriding the owner with `parent_program_id` and remapping ATA -> eATA
    /// if applicable.
    pub fn from_account_shared(
        pubkey: Pubkey,
        account_shared: &AccountSharedData,
        parent_program_id: Option<Pubkey>,
    ) -> Self {
        let remote_slot = account_shared.remote_slot();
        if let Some((eata_pubkey, eata)) =
            try_remap_ata_to_eata(&pubkey, account_shared)
        {
            return CommittedAccount {
                pubkey: eata_pubkey,
                account: eata.into(),
                remote_slot,
            };
        }

        let mut account: Account = account_shared.to_owned().into();
        account.owner = parent_program_id.unwrap_or(account.owner);

        CommittedAccount {
            pubkey,
            account,
            remote_slot,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommitType {
    /// Regular commit without actions
    Standalone(Vec<CommittedAccount>), // accounts to commit
    /// Commits accounts and runs actions
    WithBaseActions {
        committed_accounts: Vec<CommittedAccount>,
        base_actions: Vec<BaseAction>,
    },
}

impl CommitType {
    fn validate_accounts(
        accounts: &[CommittedAccountRef],
        context: &ConstructionContext<'_, '_>,
    ) -> Result<(), InstructionError> {
        accounts.iter().try_for_each(|(pubkey, account)| {
            let account_shared = account.borrow();
            if !account_shared.delegated() {
                ic_msg!(
                    context.invoke_context,
                    "ScheduleCommit ERR: account {} is required to be delegated to the current validator, in order to be committed",
                    pubkey
                );
                return Err(InstructionError::IllegalOwner)
            }

            // Validate committed account was scheduled by valid authority
            let owner = *account_shared.owner();
            validate_commit_schedule_permissions(
                &context.invoke_context,
                &owner,
                pubkey,
                context.parent_program_id.as_ref(),
                context.signers,
            )
        })
    }

    // I delegated an account, now the owner is delegation program
    // parent_program_id != Some(&acc_owner) should fail. or any modification on ER
    // ER perceives owner as old one, hence for ER those are valid txs
    // On commit_and_undelegate and commit we will set owner to DLP, for latter temporarily
    // The owner shall be real owner on chain
    // So first:
    // 1. Validate
    // 2. Fetch current account states
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
        args: CommitTypeArgs,
        context: &ConstructionContext<'_, '_>,
    ) -> Result<CommitType, InstructionError> {
        match args {
            CommitTypeArgs::Standalone(accounts) => {
                let committed_accounts_ref = Self::extract_commit_accounts(
                    &accounts,
                    context.transaction_context,
                )?;
                Self::validate_accounts(&committed_accounts_ref, context)?;
                let committed_accounts = committed_accounts_ref
                    .into_iter()
                    .map(|(pubkey, account_ref)| {
                        CommittedAccount::from_account_shared(
                            pubkey,
                            &account_ref.borrow(),
                            context.parent_program_id,
                        )
                    })
                    .collect();

                Ok(CommitType::Standalone(committed_accounts))
            }
            CommitTypeArgs::WithBaseActions {
                committed_accounts,
                base_actions,
            } => {
                let committed_accounts_ref = Self::extract_commit_accounts(
                    &committed_accounts,
                    context.transaction_context,
                )?;
                Self::validate_accounts(&committed_accounts_ref, context)?;

                let base_actions = base_actions
                    .into_iter()
                    .map(|args| BaseAction::try_from_args(args, context))
                    .collect::<Result<Vec<BaseAction>, InstructionError>>()?;
                let committed_accounts = committed_accounts_ref
                    .into_iter()
                    .map(|(pubkey, account_ref)| {
                        CommittedAccount::from_account_shared(
                            pubkey,
                            &account_ref.borrow(),
                            context.parent_program_id,
                        )
                    })
                    .collect();

                Ok(CommitType::WithBaseActions {
                    committed_accounts,
                    base_actions,
                })
            }
        }
    }

    pub fn get_committed_accounts(&self) -> &Vec<CommittedAccount> {
        match self {
            Self::Standalone(committed_accounts) => committed_accounts,
            Self::WithBaseActions {
                committed_accounts, ..
            } => committed_accounts,
        }
    }

    pub fn get_committed_accounts_mut(&mut self) -> &mut Vec<CommittedAccount> {
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
        args: UndelegateTypeArgs,
        context: &ConstructionContext<'_, '_>,
    ) -> Result<UndelegateType, InstructionError> {
        match args {
            UndelegateTypeArgs::Standalone => Ok(UndelegateType::Standalone),
            UndelegateTypeArgs::WithBaseActions { base_actions } => {
                let base_actions = base_actions
                    .into_iter()
                    .map(|base_action| {
                        BaseAction::try_from_args(base_action, context)
                    })
                    .collect::<Result<Vec<BaseAction>, InstructionError>>()?;
                Ok(UndelegateType::WithBaseActions(base_actions))
            }
        }
    }
}

/// Validate that a committee account has a permission to be committed.
///
/// Invariants:
/// - The account must be owned by the parent program *or*
/// - The account pubkey must be a signer *or*
/// - The validator authority must have signed the transaction.
///
/// If none of the above holds, returns `InvalidInstructionData` when
/// the parent program id cannot be determined, or `InvalidAccountOwner`
/// when the owner does not match the invoking program.
pub(crate) fn validate_commit_schedule_permissions(
    invoke_context: &&mut InvokeContext,
    committee_owner: &Pubkey,
    committee_pubkey: &Pubkey,
    parent_program_id: Option<&Pubkey>,
    signers: &HashSet<Pubkey>,
) -> Result<(), InstructionError> {
    let validator_id = validator_authority_id();
    let is_eata_token_program_call = parent_program_id
        == Some(&EATA_PROGRAM_ID)
        && committee_owner == &TOKEN_PROGRAM_ID;
    if parent_program_id != Some(committee_owner)
        && !signers.contains(committee_pubkey)
        && !signers.contains(&validator_id)
        && !is_eata_token_program_call
    {
        match parent_program_id {
            None => {
                ic_msg!(
                    invoke_context,
                    "ScheduleCommit ERR: failed to find parent program id"
                );
                Err(InstructionError::InvalidInstructionData)
            }
            Some(parent_id) => {
                ic_msg!(
                    invoke_context,
                    "ScheduleCommit ERR: account {} needs to be owned by the invoking program {}, be a signer, or ix must be signed by the validator to be committed, but is owned by {}",
                    committee_pubkey, parent_id, committee_owner
                );
                Err(InstructionError::InvalidAccountOwner)
            }
        }
    } else {
        Ok(())
    }
}
