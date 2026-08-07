use std::collections::HashMap;

use magicblock_magic_program_api::args::{ActionArgs, ShortAccountMeta};
use serde::{Deserialize, Serialize};
use solana_program::instruction::InstructionError;
use solana_pubkey::Pubkey;
pub use types::CommittedAccount;
use wincode::{SchemaRead, SchemaWrite};
pub mod types;

/// Commits that are covered by User's dlp PDAs
pub const ACTUAL_COMMIT_LIMIT: u64 = 25;
/// Fixed fee per commit.
/// https://github.com/magicblock-labs/delegation-program/blob/main/src/consts.rs#L11
pub const COMMIT_FEE_LAMPORTS: u64 = 100_000;
/// Price per compute unit for a BaseAction executed on Solana base chain,
/// denominated in micro-lamports per CU (mirrors Solana's priority fee model).
pub const COMPUTE_UNIT_PRICE_MICRO_LAMPORTS: u64 = 50_000;
pub const MISSING_COMMIT_NONCE_ERR: u32 = 0xA000_0001;

// BaseIntent user wants to send to base layer
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, SchemaRead, SchemaWrite,
)]
pub enum MagicBaseIntent {
    /// Actions without commitment or undelegation
    BaseActions(Vec<BaseAction>),
    Commit(CommitType),
    CommitAndUndelegate(CommitAndUndelegate),
    CommitFinalize(CommitType),
    CommitFinalizeAndUndelegate(CommitAndUndelegate),
}

// Bundle of BaseIntents
#[derive(
    Debug,
    Default,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    SchemaRead,
    SchemaWrite,
)]
pub struct MagicIntentBundle {
    pub commit: Option<CommitType>,
    pub commit_and_undelegate: Option<CommitAndUndelegate>,
    pub commit_finalize: Option<CommitType>,
    pub commit_finalize_and_undelegate: Option<CommitAndUndelegate>,
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
            MagicBaseIntent::CommitFinalize(value) => {
                this.commit_finalize = Some(value)
            }
            MagicBaseIntent::CommitFinalizeAndUndelegate(value) => {
                this.commit_finalize_and_undelegate = Some(value)
            }
        }

        this
    }
}

impl MagicIntentBundle {
    pub fn calculate_fee(
        &self,
        commit_nonces: &HashMap<Pubkey, u64>,
    ) -> Result<u64, InstructionError> {
        let mut fee = 0;
        if let Some(ref commit) = self.commit {
            fee += commit.calculate_fee(commit_nonces)?;
        }
        if let Some(ref cau) = self.commit_and_undelegate {
            fee += cau.calculate_fee(commit_nonces)?;
        }
        if let Some(ref commit_finalize) = self.commit_finalize {
            fee += commit_finalize.calculate_fee(commit_nonces)?;
        }
        if let Some(ref cfau) = self.commit_finalize_and_undelegate {
            fee += cfau.calculate_fee(commit_nonces)?;
        }
        fee += calculate_actions_fee(&self.standalone_actions);
        Ok(fee)
    }

    pub fn has_undelegate_intent(&self) -> bool {
        self.commit_and_undelegate.is_some()
            || self.commit_finalize_and_undelegate.is_some()
    }

    pub fn has_committed_accounts(&self) -> bool {
        let has_commit_intent_accounts = self
            .get_commit_intent_accounts()
            .map(|el| !el.is_empty())
            .unwrap_or(false);
        let has_undelegate_intent_accounts = self
            .get_undelegate_intent_accounts()
            .map(|el| !el.is_empty())
            .unwrap_or(false);
        let has_commit_finalize_intent_accounts = self
            .get_commit_finalize_intent_accounts()
            .map(|el| !el.is_empty())
            .unwrap_or(false);
        let has_commit_finalize_and_undelegate_intent_accounts = self
            .get_commit_finalize_and_undelegate_intent_accounts()
            .map(|el| !el.is_empty())
            .unwrap_or(false);

        has_commit_intent_accounts
            || has_undelegate_intent_accounts
            || has_commit_finalize_intent_accounts
            || has_commit_finalize_and_undelegate_intent_accounts
    }

    /// Returns `[CommitAndUndelegate]` intent's accounts
    pub fn get_undelegate_intent_accounts(
        &self,
    ) -> Option<&Vec<CommittedAccount>> {
        Some(
            self.commit_and_undelegate
                .as_ref()?
                .get_committed_accounts(),
        )
    }

    /// Returns `Commit` intent's accounts
    pub fn get_commit_intent_accounts(&self) -> Option<&Vec<CommittedAccount>> {
        Some(self.commit.as_ref()?.get_committed_accounts())
    }

    /// Returns `[CommitAndUndelegate]` intent's accounts
    pub fn get_commit_finalize_and_undelegate_intent_accounts(
        &self,
    ) -> Option<&Vec<CommittedAccount>> {
        Some(
            self.commit_finalize_and_undelegate
                .as_ref()?
                .get_committed_accounts(),
        )
    }

    /// Returns `Commit` intent's accounts
    pub fn get_commit_finalize_intent_accounts(
        &self,
    ) -> Option<&Vec<CommittedAccount>> {
        Some(self.commit_finalize.as_ref()?.get_committed_accounts())
    }

    /// Returns `Commit` intent's accounts
    pub fn get_commit_intent_accounts_mut(
        &mut self,
    ) -> Option<&mut Vec<CommittedAccount>> {
        Some(self.commit.as_mut()?.get_committed_accounts_mut())
    }

    /// Returns all the accounts that will be committed,
    /// including the ones that will be undelegated as well
    pub fn get_all_committed_accounts(&self) -> Vec<CommittedAccount> {
        let committed = self.get_commit_intent_accounts();
        let undelegated = self.get_undelegate_intent_accounts();
        let commit_finalize = self.get_commit_finalize_intent_accounts();
        let commit_finalize_and_undelegate =
            self.get_commit_finalize_and_undelegate_intent_accounts();

        [
            committed,
            undelegated,
            commit_finalize,
            commit_finalize_and_undelegate,
        ]
        .into_iter()
        .flatten()
        .flatten()
        .cloned()
        .collect()
    }

    pub fn get_all_committed_pubkeys(&self) -> Vec<Pubkey> {
        [
            self.get_commit_intent_pubkeys(),
            self.get_undelegate_intent_pubkeys(),
            self.get_commit_finalize_intent_pubkeys(),
            self.get_commit_finalize_and_undelegate_intent_pubkeys(),
        ]
        .into_iter()
        .flatten()
        .flatten()
        .collect()
    }

    pub fn get_commit_intent_pubkeys(&self) -> Option<Vec<Pubkey>> {
        self.commit
            .as_ref()
            .map(|value| value.get_committed_pubkeys())
    }

    pub fn get_undelegate_intent_pubkeys(&self) -> Option<Vec<Pubkey>> {
        self.commit_and_undelegate
            .as_ref()
            .map(|value| value.get_committed_pubkeys())
    }

    pub fn get_commit_finalize_intent_pubkeys(&self) -> Option<Vec<Pubkey>> {
        self.commit_finalize
            .as_ref()
            .map(|value| value.get_committed_pubkeys())
    }

    pub fn get_commit_finalize_and_undelegate_intent_pubkeys(
        &self,
    ) -> Option<Vec<Pubkey>> {
        self.commit_finalize_and_undelegate
            .as_ref()
            .map(|value| value.get_committed_pubkeys())
    }

    pub fn is_empty(&self) -> bool {
        let no_committed =
            self.commit.as_ref().map(|el| el.is_empty()).unwrap_or(true);

        let no_committed_and_undelegated = self
            .commit_and_undelegate
            .as_ref()
            .map(|el| el.is_empty())
            .unwrap_or(true);

        let no_commit_finalize = self
            .commit_finalize
            .as_ref()
            .map(|el| el.is_empty())
            .unwrap_or(true);

        let no_commit_finalize_and_undelegate = self
            .commit_finalize_and_undelegate
            .as_ref()
            .map(|el| el.is_empty())
            .unwrap_or(true);

        let no_actions = self.standalone_actions.is_empty();

        no_committed
            && no_committed_and_undelegated
            && no_commit_finalize
            && no_commit_finalize_and_undelegate
            && no_actions
    }

    pub fn has_callbacks(&self) -> bool {
        let x = self
            .commit
            .as_ref()
            .map(|el| el.has_callbacks())
            .unwrap_or(false);
        let y = self
            .commit_and_undelegate
            .as_ref()
            .map(|el| el.has_callbacks())
            .unwrap_or(false);
        let cf = self
            .commit_finalize
            .as_ref()
            .map(|el| el.has_callbacks())
            .unwrap_or(false);
        let cfau = self
            .commit_finalize_and_undelegate
            .as_ref()
            .map(|el| el.has_callbacks())
            .unwrap_or(false);
        let z = self
            .standalone_actions
            .iter()
            .any(|el| el.callback.is_some());

        x || y || cf || cfau || z
    }

    pub fn get_action_mut(&mut self, id: u64) -> Option<&mut BaseAction> {
        if let Some(commit) = self.commit.as_mut()
            && let Some(action) = commit.get_action_mut(id)
        {
            return Some(action);
        }

        if let Some(cau) = self.commit_and_undelegate.as_mut()
            && let Some(action) = cau.get_action_mut(id)
        {
            return Some(action);
        }

        if let Some(action) =
            self.standalone_actions.iter_mut().find(|a| a.id == id)
        {
            return Some(action);
        }

        if let Some(commit_finalize) = self.commit_finalize.as_mut()
            && let Some(action) = commit_finalize.get_action_mut(id)
        {
            return Some(action);
        }

        if let Some(cfau) = self.commit_finalize_and_undelegate.as_mut()
            && let Some(action) = cfau.get_action_mut(id)
        {
            return Some(action);
        }

        None
    }
}

impl MagicBaseIntent {
    pub fn is_undelegate(&self) -> bool {
        match &self {
            MagicBaseIntent::BaseActions(_) => false,
            MagicBaseIntent::Commit(_) => false,
            MagicBaseIntent::CommitAndUndelegate(_) => true,
            MagicBaseIntent::CommitFinalize(_) => false,
            MagicBaseIntent::CommitFinalizeAndUndelegate(_) => true,
        }
    }

    pub fn is_commit_finalize(&self) -> bool {
        match &self {
            MagicBaseIntent::BaseActions(_) => false,
            MagicBaseIntent::Commit(_) => false,
            MagicBaseIntent::CommitAndUndelegate(_) => false,
            MagicBaseIntent::CommitFinalize(_) => true,
            MagicBaseIntent::CommitFinalizeAndUndelegate(_) => true,
        }
    }

    pub fn get_committed_accounts(&self) -> Option<&Vec<CommittedAccount>> {
        match self {
            MagicBaseIntent::BaseActions(_) => None,
            MagicBaseIntent::Commit(t) => Some(t.get_committed_accounts()),
            MagicBaseIntent::CommitAndUndelegate(t) => {
                Some(t.get_committed_accounts())
            }
            MagicBaseIntent::CommitFinalize(t) => {
                Some(t.get_committed_accounts())
            }
            MagicBaseIntent::CommitFinalizeAndUndelegate(t) => {
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
            MagicBaseIntent::CommitFinalize(t) => {
                Some(t.get_committed_accounts_mut())
            }
            MagicBaseIntent::CommitFinalizeAndUndelegate(t) => {
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
            MagicBaseIntent::CommitFinalize(t) => t.is_empty(),
            MagicBaseIntent::CommitFinalizeAndUndelegate(t) => t.is_empty(),
        }
    }
}

#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, SchemaRead, SchemaWrite,
)]
pub struct CommitAndUndelegate {
    pub commit_action: CommitType,
    pub undelegate_action: UndelegateType,
}

impl CommitAndUndelegate {
    pub fn calculate_fee(
        &self,
        commit_nonces: &HashMap<Pubkey, u64>,
    ) -> Result<u64, InstructionError> {
        let mut fee = 0;
        fee += self.commit_action.calculate_fee(commit_nonces)?;
        fee += self.undelegate_action.calculate_fee(commit_nonces)?;
        Ok(fee)
    }

    pub fn get_committed_accounts(&self) -> &Vec<CommittedAccount> {
        self.commit_action.get_committed_accounts()
    }

    pub fn get_committed_accounts_mut(&mut self) -> &mut Vec<CommittedAccount> {
        self.commit_action.get_committed_accounts_mut()
    }

    pub fn get_committed_pubkeys(&self) -> Vec<Pubkey> {
        self.commit_action.get_committed_pubkeys()
    }

    pub fn is_empty(&self) -> bool {
        self.commit_action.is_empty()
    }

    pub fn has_callbacks(&self) -> bool {
        let x = self.commit_action.has_callbacks();
        let y = if let UndelegateType::WithBaseActions(actions) =
            &self.undelegate_action
        {
            actions.iter().any(|el| el.callback.is_some())
        } else {
            false
        };

        x || y
    }

    pub fn get_action_mut(&mut self, id: u64) -> Option<&mut BaseAction> {
        if let Some(action) = self.commit_action.get_action_mut(id) {
            Some(action)
        } else {
            self.undelegate_action.get_action_mut(id)
        }
    }
}

#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, SchemaRead, SchemaWrite,
)]
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

#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, SchemaRead, SchemaWrite,
)]
pub struct BaseAction {
    /// Stable identity of this action within its intent bundle, used to
    /// address it independently of its position (e.g. for patch removal).
    pub id: u64,
    pub compute_units: u32,
    pub destination_program: Pubkey,
    pub source_program: Option<Pubkey>,
    pub escrow_authority: Pubkey,
    pub data_per_program: ProgramArgs,
    pub account_metas_per_program: Vec<ShortAccountMeta>,
    pub callback: Option<BaseActionCallback>,
}

/// A callback that is execution with result of BaseAction
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, SchemaRead, SchemaWrite,
)]
pub struct BaseActionCallback {
    pub destination_program: Pubkey,
    pub discriminator: Vec<u8>,
    pub payload: Vec<u8>,
    pub compute_units: u32,
    pub account_metas_per_program: Vec<ShortAccountMeta>,
}

#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, SchemaRead, SchemaWrite,
)]
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
    /// Calculate fee commits
    pub fn calculate_fee(
        &self,
        commit_nonces: &HashMap<Pubkey, u64>,
    ) -> Result<u64, InstructionError> {
        let mut fee = 0;
        match self {
            CommitType::Standalone(committed_accounts) => {
                fee += calculate_commit_fee(committed_accounts, commit_nonces)?;
            }
            CommitType::WithBaseActions {
                committed_accounts,
                base_actions,
            } => {
                fee += calculate_commit_fee(committed_accounts, commit_nonces)?;
                fee += calculate_actions_fee(base_actions);
            }
        }

        Ok(fee)
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

    pub fn get_committed_pubkeys(&self) -> Vec<Pubkey> {
        self.get_committed_accounts()
            .iter()
            .map(|account| account.pubkey)
            .collect()
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

    pub fn has_callbacks(&self) -> bool {
        if let Self::WithBaseActions {
            committed_accounts: _,
            base_actions,
        } = self
        {
            base_actions.iter().any(|el| el.callback.is_some())
        } else {
            false
        }
    }

    pub fn get_action_mut(&mut self, id: u64) -> Option<&mut BaseAction> {
        if let Self::WithBaseActions { base_actions, .. } = self {
            base_actions.iter_mut().find(|a| a.id == id)
        } else {
            None
        }
    }
}

/// No CommitedAccounts since it is only used with CommitAction.
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, SchemaRead, SchemaWrite,
)]
pub enum UndelegateType {
    Standalone,
    WithBaseActions(Vec<BaseAction>),
}

impl UndelegateType {
    pub fn get_action_mut(&mut self, id: u64) -> Option<&mut BaseAction> {
        if let Self::WithBaseActions(actions) = self {
            actions.iter_mut().find(|a| a.id == id)
        } else {
            None
        }
    }

    pub fn calculate_fee(
        &self,
        _commit_nonces: &HashMap<Pubkey, u64>,
    ) -> Result<u64, InstructionError> {
        match self {
            UndelegateType::Standalone => Ok(0),
            UndelegateType::WithBaseActions(actions) => {
                Ok(calculate_actions_fee(actions))
            }
        }
    }
}

pub fn calculate_commit_fee(
    accounts: &[CommittedAccount],
    commit_nonces: &HashMap<Pubkey, u64>,
) -> Result<u64, InstructionError> {
    accounts.iter().try_fold(0u64, |fee, account| {
        if let Some(nonce) = commit_nonces.get(&account.pubkey) {
            if nonce >= &ACTUAL_COMMIT_LIMIT {
                Ok(fee + COMMIT_FEE_LAMPORTS)
            } else {
                Ok(fee)
            }
        } else {
            Err(InstructionError::Custom(MISSING_COMMIT_NONCE_ERR))
        }
    })
}

fn calculate_actions_fee(actions: &[BaseAction]) -> u64 {
    const MICRO_LAMPORTS_PER_LAMPORT: u64 = 1_000_000;
    let micro_lamports = actions.iter().fold(0u64, |acc, action| {
        acc.saturating_add(
            action.compute_units as u64 * COMPUTE_UNIT_PRICE_MICRO_LAMPORTS,
        )
    });
    micro_lamports.div_ceil(MICRO_LAMPORTS_PER_LAMPORT)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use solana_program::instruction::InstructionError;
    use solana_pubkey::Pubkey;

    use crate::intent::{
        ACTUAL_COMMIT_LIMIT, BaseAction, BaseActionCallback,
        COMMIT_FEE_LAMPORTS, CommitAndUndelegate, CommitType,
        MISSING_COMMIT_NONCE_ERR, MagicIntentBundle, ProgramArgs,
        UndelegateType, calculate_actions_fee, calculate_commit_fee,
        types::CommittedAccount,
    };

    fn make_committed_account(pubkey: Pubkey) -> CommittedAccount {
        CommittedAccount {
            pubkey,
            account: solana_account::Account::default(),
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

    // ---- calculate_commit_fee ----

    #[test]
    fn test_commit_fee_at_limit_is_zero() {
        let pk = Pubkey::new_unique();
        // nonce is commits done so far; nonce+1 is the next commit number.
        // ACTUAL_COMMIT_LIMIT - 1 means the next commit is exactly at the limit → free.
        let nonces = HashMap::from([(pk, ACTUAL_COMMIT_LIMIT - 1)]);
        let fee = calculate_commit_fee(&[make_committed_account(pk)], &nonces)
            .unwrap();
        assert_eq!(fee, 0);
    }

    #[test]
    fn test_commit_fee_above_limit_charges_per_account() {
        let pk1 = Pubkey::new_unique();
        let pk2 = Pubkey::new_unique();
        let nonces = HashMap::from([
            (pk1, ACTUAL_COMMIT_LIMIT + 1),
            (pk2, ACTUAL_COMMIT_LIMIT + 1),
        ]);
        let fee = calculate_commit_fee(
            &[make_committed_account(pk1), make_committed_account(pk2)],
            &nonces,
        )
        .unwrap();
        assert_eq!(fee, COMMIT_FEE_LAMPORTS * 2);
    }

    #[test]
    fn test_commit_fee_mixed_accounts() {
        let pk_below = Pubkey::new_unique();
        let pk_above = Pubkey::new_unique();
        let nonces = HashMap::from([
            (pk_below, ACTUAL_COMMIT_LIMIT - 1), // next commit is exactly at limit → free
            (pk_above, ACTUAL_COMMIT_LIMIT), // next commit exceeds limit → charged
        ]);
        let fee = calculate_commit_fee(
            &[
                make_committed_account(pk_below),
                make_committed_account(pk_above),
            ],
            &nonces,
        )
        .unwrap();
        assert_eq!(fee, COMMIT_FEE_LAMPORTS);
    }

    #[test]
    fn test_commit_fee_missing_nonce_errors() {
        let pk = Pubkey::new_unique();
        let err = calculate_commit_fee(
            &[make_committed_account(pk)],
            &HashMap::new(),
        )
        .unwrap_err();
        assert_eq!(err, InstructionError::Custom(MISSING_COMMIT_NONCE_ERR));
    }

    /// two actions of 200_000 CUs each → 20_000 lamports
    #[test]
    fn test_actions_fee_multiple_actions() {
        assert_eq!(
            calculate_actions_fee(&[
                make_base_action(200_000),
                make_base_action(200_000)
            ]),
            20_000
        );
    }

    // ---- MagicIntentBundle::calculate_fee / has_callbacks / get_action_mut
    // must include commit_finalize and commit_finalize_and_undelegate ----

    #[test]
    fn test_calculate_fee_includes_commit_finalize_variants() {
        let pk = Pubkey::new_unique();
        let nonces = HashMap::from([(pk, ACTUAL_COMMIT_LIMIT)]); // over limit -> charged
        let expected = COMMIT_FEE_LAMPORTS + 10_000; // + one 200_000 CU action

        let commit_finalize_bundle = MagicIntentBundle {
            commit_finalize: Some(CommitType::WithBaseActions {
                committed_accounts: vec![make_committed_account(pk)],
                base_actions: vec![make_base_action(200_000)],
            }),
            ..Default::default()
        };
        assert_eq!(
            commit_finalize_bundle.calculate_fee(&nonces).unwrap(),
            expected
        );

        let commit_finalize_and_undelegate_bundle = MagicIntentBundle {
            commit_finalize_and_undelegate: Some(CommitAndUndelegate {
                commit_action: CommitType::Standalone(vec![
                    make_committed_account(pk),
                ]),
                undelegate_action: UndelegateType::WithBaseActions(vec![
                    make_base_action(200_000),
                ]),
            }),
            ..Default::default()
        };
        assert_eq!(
            commit_finalize_and_undelegate_bundle
                .calculate_fee(&nonces)
                .unwrap(),
            expected
        );
    }

    #[test]
    fn test_has_callbacks_detects_commit_finalize_variants() {
        fn make_base_action_with_callback() -> BaseAction {
            let mut action = make_base_action(0);
            action.callback = Some(BaseActionCallback {
                destination_program: Pubkey::new_unique(),
                discriminator: vec![],
                payload: vec![],
                compute_units: 0,
                account_metas_per_program: vec![],
            });
            action
        }

        let commit_finalize_bundle = MagicIntentBundle {
            commit_finalize: Some(CommitType::WithBaseActions {
                committed_accounts: vec![],
                base_actions: vec![make_base_action_with_callback()],
            }),
            ..Default::default()
        };
        assert!(commit_finalize_bundle.has_callbacks());

        let commit_finalize_and_undelegate_bundle = MagicIntentBundle {
            commit_finalize_and_undelegate: Some(CommitAndUndelegate {
                commit_action: CommitType::Standalone(vec![]),
                undelegate_action: UndelegateType::WithBaseActions(vec![
                    make_base_action_with_callback(),
                ]),
            }),
            ..Default::default()
        };
        assert!(commit_finalize_and_undelegate_bundle.has_callbacks());

        // No callbacks anywhere -> false, so the above aren't vacuously true.
        let no_callbacks_bundle = MagicIntentBundle {
            commit_finalize: Some(CommitType::WithBaseActions {
                committed_accounts: vec![],
                base_actions: vec![make_base_action(0)],
            }),
            ..Default::default()
        };
        assert!(!no_callbacks_bundle.has_callbacks());
    }

    #[test]
    fn test_get_action_mut_finds_by_id_across_commit_finalize_variants() {
        let mut commit_action = make_base_action(111);
        commit_action.id = 10;
        let mut commit_finalize_action = make_base_action(222);
        commit_finalize_action.id = 20;
        let mut standalone_action = make_base_action(333);
        standalone_action.id = 30;

        let mut bundle = MagicIntentBundle {
            commit: Some(CommitType::WithBaseActions {
                committed_accounts: vec![],
                base_actions: vec![commit_action],
            }),
            commit_finalize: Some(CommitType::WithBaseActions {
                committed_accounts: vec![],
                base_actions: vec![commit_finalize_action],
            }),
            standalone_actions: vec![standalone_action],
            ..Default::default()
        };

        assert_eq!(bundle.get_action_mut(10).unwrap().compute_units, 111);
        assert_eq!(bundle.get_action_mut(20).unwrap().compute_units, 222);
        assert_eq!(bundle.get_action_mut(30).unwrap().compute_units, 333);
        assert!(bundle.get_action_mut(999).is_none());
    }
}
