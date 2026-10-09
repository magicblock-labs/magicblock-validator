use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use solana_hash::Hash;
use solana_program::instruction::InstructionError;
use solana_pubkey::Pubkey;
use solana_transaction::Transaction;

use super::{BaseAction, CommittedAccount, MagicIntentBundle};
use crate::Slot;

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

#[cfg(test)]
mod tests {
    use solana_account::Account;

    use super::*;
    use crate::intent::CommitType;

    #[test]
    fn scheduled_intent_bundle_serialization_compatibility() {
        // Captured from the Magic Program's original ScheduledIntentBundle.
        let bytes = include_bytes!("fixtures/scheduled_intent_bundle.bin");
        let expected = ScheduledIntentBundle {
            id: 42,
            slot: 123,
            blockhash: Hash::new_from_array([3; 32]),
            sent_transaction: Transaction::default(),
            payer: Pubkey::new_from_array([4; 32]),
            intent_bundle: MagicIntentBundle {
                commit: Some(CommitType::Standalone(vec![CommittedAccount {
                    pubkey: Pubkey::new_from_array([5; 32]),
                    account: Account {
                        lamports: 987,
                        data: vec![6, 7, 8],
                        owner: Pubkey::new_from_array([9; 32]),
                        ..Default::default()
                    },
                    remote_slot: 456,
                }])),
                ..Default::default()
            },
        };

        let decoded: ScheduledIntentBundle =
            bincode::deserialize(bytes).unwrap();
        assert_eq!(decoded, expected);
        assert_eq!(bincode::serialize(&expected).unwrap(), bytes);
    }
}
