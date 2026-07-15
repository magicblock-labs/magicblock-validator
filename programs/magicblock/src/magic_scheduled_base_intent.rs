use std::collections::{HashMap, HashSet};

pub use magicblock_core::intent::{
    calculate_commit_fee, BaseAction, CommitAndUndelegate, CommitType,
    MagicBaseIntent, MagicIntentBundle, ProgramArgs, UndelegateType,
    ACTUAL_COMMIT_LIMIT, COMMIT_FEE_LAMPORTS,
    COMPUTE_UNIT_PRICE_MICRO_LAMPORTS,
};
use magicblock_core::{
    intent::types::CommittedAccount,
    token_programs::{
        EATA_PROGRAM_ID, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID,
    },
    Slot,
};
use magicblock_magic_program_api::args::{
    BaseActionArgs, CommitAndUndelegateArgs, CommitTypeArgs,
    MagicBaseIntentArgs, MagicIntentBundleArgs, UndelegateTypeArgs,
};
use serde::{Deserialize, Serialize};
use solana_account::ReadableAccount;
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
    magic_sys::validate_intent_size,
    utils::accounts::{
        get_instruction_account_with_idx, get_instruction_pubkey_with_idx,
        get_writable_with_idx, InstructionAccount,
    },
    validator::effective_validator_authority_id,
};

/// Context necessary for construction of Schedule Action
pub struct ConstructionContext<'a, 'ic, 'ix_data> {
    parent_program_id: Option<Pubkey>,
    signers: &'a HashSet<Pubkey>,
    pub invoke_context: &'a mut InvokeContext<'ic, 'ix_data>,
    /// When `true`, actions use `call_handler_v2` which passes `source_program`
    /// to the delegation program. Legacy `ScheduleBaseIntent` sets this to
    /// `false` for backward compatibility with old deployed contracts.
    pub secure: bool,
    /// Next id to assign to a `BaseAction` being constructed. Incremented
    /// as actions are built, so each action in a bundle gets a unique,
    /// stable id in construction order.
    next_action_id: u64,
}

impl<'a, 'ic, 'ix_data> ConstructionContext<'a, 'ic, 'ix_data> {
    pub fn new(
        parent_program_id: Option<Pubkey>,
        signers: &'a HashSet<Pubkey>,
        invoke_context: &'a mut InvokeContext<'ic, 'ix_data>,
        secure: bool,
    ) -> Self {
        Self {
            parent_program_id,
            signers,
            invoke_context,
            secure,
            next_action_id: 0,
        }
    }

    pub fn transaction_context(&self) -> &TransactionContext<'ix_data> {
        &*self.invoke_context.transaction_context
    }

    fn next_action_id(&mut self) -> u64 {
        let id = self.next_action_id;
        self.next_action_id += 1;
        id
    }
}

type CommitAccountRef<'a, 'ix_data> =
    (Pubkey, InstructionAccount<'a, 'ix_data>);

/// Scheduled action to be executed on base layer
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledIntentBundle {
    pub id: u64,
    pub slot: Slot,
    pub blockhash: Hash,
    pub sent_transaction: Transaction,
    pub payer: Pubkey,
    /// Scheduled intent bundle
    pub intent_bundle: MagicIntentBundle,
}

impl ScheduledIntentBundle {
    pub fn try_new(
        args: MagicIntentBundleArgs,
        commit_id: u64,
        slot: Slot,
        payer_pubkey: &Pubkey,
        context: &mut ConstructionContext<'_, '_, '_>,
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
            sent_transaction: intent_bundle_sent_transaction,
            intent_bundle,
        })
    }

    /// Calculates fee for intent
    pub fn calculate_fee(
        &self,
        commit_nonces: &HashMap<Pubkey, u64>,
    ) -> Result<u64, InstructionError> {
        const SCHEDULING_FEE: u64 = 0;

        Ok({
            SCHEDULING_FEE + self.intent_bundle.calculate_fee(commit_nonces)?
        })
    }

    /// Returns all accounts that will be committed on Base layer,
    /// including the one scheduled for undelegation
    pub fn get_all_committed_accounts(&self) -> Vec<CommittedAccount> {
        self.intent_bundle.get_all_committed_accounts()
    }

    /// Returns pubkeys of all accounts that will be committed on Base layer,
    /// including the one scheduled for undelegation
    pub fn get_all_committed_pubkeys(&self) -> Vec<Pubkey> {
        self.intent_bundle.get_all_committed_pubkeys()
    }

    /// Return `true` if there're account that will be committed on Base layer
    pub fn has_committed_accounts(&self) -> bool {
        self.intent_bundle.has_committed_accounts()
    }

    /// Returns `[CommitAndUndelegate]` intent's accounts
    pub fn get_undelegate_intent_accounts(
        &self,
    ) -> Option<&Vec<CommittedAccount>> {
        self.intent_bundle.get_undelegate_intent_accounts()
    }

    /// Returns `Commit` intent's accounts
    pub fn get_commit_intent_accounts(&self) -> Option<&Vec<CommittedAccount>> {
        self.intent_bundle.get_commit_intent_accounts()
    }

    /// Returns `[CommitFinalizeAndUndelegate]` intent's accounts
    pub fn get_commit_finalize_and_undelegate_intent_accounts(
        &self,
    ) -> Option<&Vec<CommittedAccount>> {
        self.intent_bundle
            .get_commit_finalize_and_undelegate_intent_accounts()
    }

    /// Returns `CommitFinalize` intent's accounts
    pub fn get_commit_finalize_intent_accounts(
        &self,
    ) -> Option<&Vec<CommittedAccount>> {
        self.intent_bundle.get_commit_finalize_intent_accounts()
    }

    /// Returns `Commit` intent's accounts
    pub fn get_commit_intent_accounts_mut(
        &mut self,
    ) -> Option<&mut Vec<CommittedAccount>> {
        self.intent_bundle.get_commit_intent_accounts_mut()
    }

    pub fn get_commit_intent_pubkeys(&self) -> Option<Vec<Pubkey>> {
        self.intent_bundle.get_commit_intent_pubkeys()
    }

    pub fn get_undelegate_intent_pubkeys(&self) -> Option<Vec<Pubkey>> {
        self.intent_bundle.get_undelegate_intent_pubkeys()
    }

    pub fn has_undelegate_intent(&self) -> bool {
        self.intent_bundle.has_undelegate_intent()
    }

    pub fn has_callbacks(&self) -> bool {
        self.intent_bundle.has_callbacks()
    }

    pub fn is_empty(&self) -> bool {
        self.intent_bundle.is_empty()
    }

    pub fn standalone_actions(&self) -> &Vec<BaseAction> {
        &self.intent_bundle.standalone_actions
    }
}

/// Constructs a type from its wire [`Args`] representation.
///
/// The corresponding data types live in [`magicblock_core::intent::types`]
/// since they need to be shared with `MagicSys` implementors, but the
/// construction logic stays here since it depends on [`ConstructionContext`]
/// (`InvokeContext`, `ic_msg!`, etc.) which only exists on-chain. A local
/// trait lets us `impl` construction for those foreign types.
pub trait TryFromArgs<A>: Sized {
    fn try_from_args(
        args: A,
        context: &mut ConstructionContext<'_, '_, '_>,
    ) -> Result<Self, InstructionError>;
}

impl TryFromArgs<MagicIntentBundleArgs> for MagicIntentBundle {
    fn try_from_args(
        args: MagicIntentBundleArgs,
        context: &mut ConstructionContext<'_, '_, '_>,
    ) -> Result<Self, InstructionError> {
        validate_magic_intent_bundle_args(&args, context)?;

        // NOTE: Order shall be identical to AddActionCallback's action ordering.
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

        let commit_finalize = args
            .commit_finalize
            .map(|value| CommitType::try_from_args(value, context))
            .transpose()?;

        let commit_finalize_and_undelegate = args
            .commit_finalize_and_undelegate
            .map(|value| CommitAndUndelegate::try_from_args(value, context))
            .transpose()?;

        let this = Self {
            commit,
            commit_and_undelegate,
            commit_finalize,
            commit_finalize_and_undelegate,
            standalone_actions: actions,
        };
        post_validate_magic_intent_bundle(&this, context)?;
        validate_intent_size(&this).inspect_err(|_| {
            ic_msg!(
                context.invoke_context,
                "ScheduleCommit ERR: intent is too large to ever fit into transaction",
            );
        })?;

        Ok(this)
    }
}

/// Cross intent validation:
/// 1. Set of committed accounts shall not overlap with
///    set of undelegated accounts
/// 2. None for now :)
fn validate_magic_intent_bundle_args(
    args: &MagicIntentBundleArgs,
    context: &ConstructionContext<'_, '_, '_>,
) -> Result<(), InstructionError> {
    let committed_set: Option<HashSet<_>> = args
        .commit
        .as_ref()
        .map(|el| el.committed_accounts_indices().iter().copied().collect());
    let Some(committed_set) = committed_set else {
        return Ok(());
    };

    args.commit_and_undelegate
        .as_ref()
        .map(|el| {
            let has_cross_reference = el
                .committed_accounts_indices()
                .iter()
                .any(|ind| committed_set.contains(ind));
            if has_cross_reference {
                ic_msg!(
                    context.invoke_context,
                    "ScheduleCommit ERR: duplicate committed account across bundle",
                );
                Err(InstructionError::InvalidInstructionData)
            } else {
                Ok(())
            }
        })
        .unwrap_or(Ok(()))
}

/// Post cross intent validation:
/// 1. Validates that all committed accounts across the entire intent bundle
///    are globally unique by pubkey.
fn post_validate_magic_intent_bundle(
    bundle: &MagicIntentBundle,
    context: &ConstructionContext<'_, '_, '_>,
) -> Result<(), InstructionError> {
    if bundle.is_empty() {
        ic_msg!(
            context.invoke_context,
            "ScheduleCommit ERR: intent bundle must not be empty.",
        );
        return Err(InstructionError::InvalidInstructionData);
    }

    let mut seen = HashSet::<Pubkey>::new();
    let mut check =
        |accounts: &Vec<CommittedAccount>| -> Result<(), InstructionError> {
            for el in accounts {
                if !seen.insert(el.pubkey) {
                    ic_msg!(
                        context.invoke_context,
                        "ScheduleCommit ERR: duplicate committed account pubkey across bundle: {}",
                        el.pubkey
                    );
                    return Err(InstructionError::InvalidInstructionData);
                }
            }
            Ok(())
        };

    if let Some(commit) = &bundle.commit {
        check(commit.get_committed_accounts())?;
    }
    if let Some(cau) = &bundle.commit_and_undelegate {
        check(cau.get_committed_accounts())?;
    }
    if let Some(commit_finalize) = &bundle.commit_finalize {
        check(commit_finalize.get_committed_accounts())?;
    }
    if let Some(commit_finalize_and_undelegate) =
        &bundle.commit_finalize_and_undelegate
    {
        check(commit_finalize_and_undelegate.get_committed_accounts())?;
    }

    Ok(())
}

impl TryFromArgs<MagicBaseIntentArgs> for MagicBaseIntent {
    fn try_from_args(
        args: MagicBaseIntentArgs,
        context: &mut ConstructionContext<'_, '_, '_>,
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
            MagicBaseIntentArgs::CommitFinalize(type_) => {
                let commit = CommitType::try_from_args(type_, context)?;
                Ok(MagicBaseIntent::CommitFinalize(commit))
            }
            MagicBaseIntentArgs::CommitFinalizeAndUndelegate(type_) => {
                let commit_and_undelegate =
                    CommitAndUndelegate::try_from_args(type_, context)?;
                Ok(MagicBaseIntent::CommitFinalizeAndUndelegate(
                    commit_and_undelegate,
                ))
            }
        }
    }
}

impl TryFromArgs<CommitAndUndelegateArgs> for CommitAndUndelegate {
    fn try_from_args(
        args: CommitAndUndelegateArgs,
        context: &mut ConstructionContext<'_, '_, '_>,
    ) -> Result<CommitAndUndelegate, InstructionError> {
        let account_indices = args.commit_type.committed_accounts_indices();
        validate_commit_and_undelegate_accounts(
            account_indices.as_slice(),
            context,
        )?;

        let commit_action =
            CommitType::try_from_args(args.commit_type, context)?;
        let undelegate_action =
            UndelegateType::try_from_args(args.undelegate_type, context)?;

        Ok(Self {
            commit_action,
            undelegate_action,
        })
    }
}

fn validate_commit_and_undelegate_accounts(
    account_indices: &[u8],
    context: &ConstructionContext<'_, '_, '_>,
) -> Result<(), InstructionError> {
    account_indices.iter().copied().try_for_each(|idx| {
        let is_writable =
            get_writable_with_idx(context.transaction_context(), idx as u16)?;
        let delegated = get_instruction_account_with_idx(
            context.transaction_context(),
            idx as u16,
        )?;
        if is_writable && delegated.borrow()?.delegated() {
            Ok(())
        } else {
            let pubkey = get_instruction_pubkey_with_idx(
                context.transaction_context(),
                idx as u16,
            )?;
            ic_msg!(
                context.invoke_context,
                "ScheduleCommit ERR: account {} is required to be writable and delegated in order to be undelegated",
                pubkey
            );
            Err(InstructionError::ReadonlyDataModified)
        }
    })
}

impl TryFromArgs<BaseActionArgs> for BaseAction {
    fn try_from_args(
        args: BaseActionArgs,
        context: &mut ConstructionContext<'_, '_, '_>,
    ) -> Result<BaseAction, InstructionError> {
        // Since action on Base layer performed on behalf of some escrow
        // We need to ensure that action was authorized by legit owner
        let authority_pubkey = *get_instruction_pubkey_with_idx(
            context.transaction_context(),
            args.escrow_authority as u16,
        )?;
        if !context.signers.contains(&authority_pubkey) {
            ic_msg!(
                context.invoke_context,
                &format!(
                    "BaseAction: authority pubkey must sign transaction: {}",
                    authority_pubkey
                )
            );

            return Err(InstructionError::MissingRequiredSignature);
        }

        let Some(parent_program_id) = context.parent_program_id else {
            ic_msg!(
                context.invoke_context,
                "BaseAction: actions can only be scheduled via CPI",
            );
            return Err(InstructionError::UnsupportedProgramId);
        };

        let source_program = if context.secure {
            Some(parent_program_id)
        } else if args.destination_program == parent_program_id {
            None
        } else {
            ic_msg!(
                context.invoke_context,
                "BaseAction: v1 can act only within same program",
            );
            return Err(InstructionError::UnsupportedProgramId);
        };

        Ok(BaseAction {
            id: context.next_action_id(),
            compute_units: args.compute_units,
            destination_program: args.destination_program,
            source_program,
            escrow_authority: authority_pubkey,
            data_per_program: args.args.into(),
            account_metas_per_program: args.accounts,
            callback: None,
        })
    }
}

fn validate_commit_type_accounts(
    accounts: &[CommitAccountRef<'_, '_>],
    context: &ConstructionContext<'_, '_, '_>,
) -> Result<(), InstructionError> {
    accounts.iter().try_for_each(|(pubkey, account)| {
        if account.to_account_shared_data()?.confined() {
            ic_msg!(
                context.invoke_context,
                "ScheduleCommit ERR: account {} is confined and cannot be committed",
                pubkey
            );
            return Err(InstructionError::InvalidAccountData);
        }

        // Prevent ephemeral accounts from being committed to base chain
        if account.to_account_shared_data()?.ephemeral() {
            ic_msg!(
                context.invoke_context,
                "ScheduleCommit ERR: account {} is ephemeral and cannot be committed to base chain",
                pubkey
            );
            return Err(InstructionError::InvalidAccountData);
        }

        if !account.borrow()?.delegated() {
            ic_msg!(
                context.invoke_context,
                "ScheduleCommit ERR: account {} is required to be delegated to the current validator, in order to be committed",
                pubkey
            );
            return Err(InstructionError::IllegalOwner)
        }

        // Validate committed account was scheduled by valid authority
        let owner = *account.borrow()?.owner();
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
pub(crate) fn extract_commit_accounts<'a, 'ix_data>(
    account_indices: &[u8],
    transaction_context: &'a TransactionContext<'ix_data>,
) -> Result<Vec<CommitAccountRef<'a, 'ix_data>>, InstructionError> {
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

impl TryFromArgs<CommitTypeArgs> for CommitType {
    fn try_from_args(
        args: CommitTypeArgs,
        context: &mut ConstructionContext<'_, '_, '_>,
    ) -> Result<CommitType, InstructionError> {
        match args {
            CommitTypeArgs::Standalone(accounts) => {
                let committed_accounts_ref = extract_commit_accounts(
                    &accounts,
                    context.transaction_context(),
                )?;
                validate_commit_type_accounts(
                    &committed_accounts_ref,
                    context,
                )?;
                let committed_accounts = committed_accounts_ref
                    .into_iter()
                    .map(|(pubkey, account)| {
                        let account = account.to_account_shared_data()?;
                        Ok(CommittedAccount::from_account_shared(
                            pubkey,
                            &account,
                            context.parent_program_id,
                        ))
                    })
                    .collect::<Result<_, InstructionError>>()?;

                Ok(CommitType::Standalone(committed_accounts))
            }
            CommitTypeArgs::WithBaseActions {
                committed_accounts,
                base_actions,
            } => {
                let committed_accounts_ref = extract_commit_accounts(
                    &committed_accounts,
                    context.transaction_context(),
                )?;
                validate_commit_type_accounts(
                    &committed_accounts_ref,
                    context,
                )?;

                let committed_accounts = committed_accounts_ref
                    .into_iter()
                    .map(|(pubkey, account)| {
                        let account = account.to_account_shared_data()?;
                        Ok(CommittedAccount::from_account_shared(
                            pubkey,
                            &account,
                            context.parent_program_id,
                        ))
                    })
                    .collect::<Result<_, InstructionError>>()?;
                let base_actions = base_actions
                    .into_iter()
                    .map(|args| BaseAction::try_from_args(args, context))
                    .collect::<Result<Vec<BaseAction>, InstructionError>>()?;

                Ok(CommitType::WithBaseActions {
                    committed_accounts,
                    base_actions,
                })
            }
        }
    }
}

impl TryFromArgs<UndelegateTypeArgs> for UndelegateType {
    fn try_from_args(
        args: UndelegateTypeArgs,
        context: &mut ConstructionContext<'_, '_, '_>,
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
    let validator_id = effective_validator_authority_id();
    let is_token_account_owner = committee_owner == &TOKEN_PROGRAM_ID
        || committee_owner == &TOKEN_2022_PROGRAM_ID;
    let is_eata_token_program_call =
        parent_program_id == Some(&EATA_PROGRAM_ID) && is_token_account_owner;
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use magicblock_core::intent::types::CommittedAccount;
    use solana_account::Account;
    use solana_pubkey::Pubkey;

    use super::*;

    fn make_committed_account(pubkey: Pubkey) -> CommittedAccount {
        CommittedAccount {
            pubkey,
            account: Account::default(),
            remote_slot: 0,
        }
    }

    fn make_base_action(compute_units: u32) -> BaseAction {
        BaseAction {
            id: 0,
            compute_units,
            destination_program: Pubkey::new_unique(),
            source_program: None,
            escrow_authority: Pubkey::new_unique(),
            data_per_program: ProgramArgs {
                escrow_index: 0,
                data: vec![],
            },
            account_metas_per_program: vec![],
            callback: None,
        }
    }

    // ---- ScheduledIntentBundle::calculate_fee ----

    #[test]
    fn test_scheduled_intent_bundle_fee_mixed_commit_and_cau() {
        use solana_hash::Hash;
        use solana_transaction::Transaction;

        // commit WithBaseActions: pk1 above limit (charged), pk2 at limit (free)
        let pk1 = Pubkey::new_unique();
        let pk2 = Pubkey::new_unique();
        // cau commit: pk3 above limit (charged); undelegate has actions too
        let pk3 = Pubkey::new_unique();

        let bundle = ScheduledIntentBundle {
            id: 0,
            slot: 0,
            blockhash: Hash::default(),
            sent_transaction: Transaction::default(),
            payer: Pubkey::new_unique(),
            intent_bundle: MagicIntentBundle {
                commit: Some(CommitType::WithBaseActions {
                    committed_accounts: vec![
                        make_committed_account(pk1),
                        make_committed_account(pk2),
                    ],
                    base_actions: vec![make_base_action(200_000)],
                }),
                commit_and_undelegate: Some(CommitAndUndelegate {
                    commit_action: CommitType::Standalone(vec![
                        make_committed_account(pk3),
                    ]),
                    undelegate_action: UndelegateType::WithBaseActions(vec![
                        make_base_action(100_000),
                    ]),
                }),
                commit_finalize: None,
                commit_finalize_and_undelegate: None,
                standalone_actions: vec![make_base_action(50_000)],
            },
        };

        let nonces = HashMap::from([
            (pk1, ACTUAL_COMMIT_LIMIT), // next commit exceeds limit → charged
            (pk2, ACTUAL_COMMIT_LIMIT - 1), // next commit is exactly at limit → free
            (pk3, ACTUAL_COMMIT_LIMIT), // next commit exceeds limit → charged
        ]);

        let fee = bundle.calculate_fee(&nonces).unwrap();

        // commit: pk1 (100_000) + action 200k CUs (10_000) = 110_000
        // cau:    pk3 (100_000) + undelegate action 100k CUs (5_000) = 105_000
        // standalone: action 50k CUs (2_500)
        let expected = COMMIT_FEE_LAMPORTS       // pk1
            + 10_000                             // 200k CU action
            + COMMIT_FEE_LAMPORTS               // pk3
            + 5_000                              // 100k CU undelegate action
            + 2_500; // 50k CU standalone action
        assert_eq!(fee, expected);
    }
}
