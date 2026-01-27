use std::mem;

use magicblock_magic_program_api::MAGIC_CONTEXT_SIZE;
use serde::{Deserialize, Serialize};
use solana_account::{AccountSharedData, ReadableAccount};

use crate::magic_scheduled_base_intent::ScheduledIntentBundle;

#[derive(Debug, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub struct MagicContext {
    pub intent_id: u64,
    pub scheduled_base_intents: Vec<ScheduledIntentBundle>,
}

impl MagicContext {
    pub const SIZE: usize = MAGIC_CONTEXT_SIZE;
    pub const ZERO: [u8; Self::SIZE] = [0; Self::SIZE];
    pub(crate) fn deserialize(
        data: &AccountSharedData,
    ) -> Result<Self, bincode::error::DecodeError> {
        if data.data().is_empty() {
            Ok(Self::default())
        } else {
            Ok(bincode::serde::decode_from_slice(
                data.data(),
                bincode::config::legacy(),
            )?
            .0)
        }
    }

    pub(crate) fn next_intent_id(&mut self) -> u64 {
        let output = self.intent_id;
        self.intent_id = self.intent_id.wrapping_add(1);

        output
    }

    pub(crate) fn add_scheduled_action(
        &mut self,
        base_intent: ScheduledIntentBundle,
    ) {
        self.scheduled_base_intents.push(base_intent);
    }

    pub(crate) fn take_scheduled_commits(
        &mut self,
    ) -> Vec<ScheduledIntentBundle> {
        mem::take(&mut self.scheduled_base_intents)
    }

    pub fn has_scheduled_commits(data: &[u8]) -> bool {
        // Currently we only store a vec of scheduled commits in the MagicContext
        // The first 8 bytes contain the length of the vec
        // This works even if the length is actually stored as a u32
        // since we zero out the entire context whenever we update the vec
        !is_zeroed(&data[8..16])
    }
}

fn is_zeroed(buf: &[u8]) -> bool {
    const VEC_SIZE_LEN: usize = 8;
    const ZEROS: [u8; VEC_SIZE_LEN] = [0; VEC_SIZE_LEN];
    let mut chunks = buf.chunks_exact(VEC_SIZE_LEN);

    #[allow(clippy::indexing_slicing)]
    {
        chunks.all(|chunk| chunk == &ZEROS[..])
            && chunks.remainder() == &ZEROS[..chunks.remainder().len()]
    }
}

#[cfg(test)]
mod bincode_compatibility_tests {
    use magicblock_magic_program_api::args::ShortAccountMeta;
    use solana_account::Account;
    use solana_hash::Hash;
    use solana_pubkey::Pubkey;
    use solana_transaction::Transaction;

    use super::*;
    use crate::magic_scheduled_base_intent::{
        BaseAction, CommitAndUndelegate, CommitType, CommittedAccount,
        MagicBaseIntent, MagicIntentBundle, ProgramArgs, ScheduledIntentBundle,
        UndelegateType,
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

    fn test_hash() -> Hash {
        Hash::new_from_array([3u8; 32])
    }

    fn test_account() -> Account {
        Account {
            lamports: 1000,
            data: vec![1, 2, 3, 4],
            owner: test_pubkey(),
            executable: false,
            rent_epoch: 0,
        }
    }

    fn test_committed_account() -> CommittedAccount {
        CommittedAccount {
            pubkey: test_pubkey(),
            account: test_account(),
            remote_slot: 100,
        }
    }

    fn test_base_action() -> BaseAction {
        BaseAction {
            compute_units: 5000,
            destination_program: test_pubkey2(),
            escrow_authority: test_pubkey(),
            data_per_program: ProgramArgs {
                escrow_index: 1,
                data: vec![10, 20, 30],
            },
            account_metas_per_program: vec![ShortAccountMeta {
                pubkey: test_pubkey(),
                is_writable: true,
            }],
        }
    }

    #[test]
    fn test_magic_context_empty_compatibility() {
        let context = MagicContext::default();
        assert_bincode_compatible(&context, "MagicContext::default");
        assert_bincode_deserialize_compatible(
            &context,
            "MagicContext::default",
        );
    }

    #[test]
    fn test_magic_context_with_intent_id_compatibility() {
        let context = MagicContext {
            intent_id: 42,
            scheduled_base_intents: vec![],
        };
        assert_bincode_compatible(&context, "MagicContext with intent_id");
        assert_bincode_deserialize_compatible(
            &context,
            "MagicContext with intent_id",
        );
    }

    #[test]
    fn test_committed_account_compatibility() {
        let account = test_committed_account();
        assert_bincode_compatible(&account, "CommittedAccount");
        assert_bincode_deserialize_compatible(&account, "CommittedAccount");
    }

    #[test]
    fn test_program_args_compatibility() {
        let args = ProgramArgs {
            escrow_index: 5,
            data: vec![100, 200, 255],
        };
        assert_bincode_compatible(&args, "ProgramArgs");
        assert_bincode_deserialize_compatible(&args, "ProgramArgs");
    }

    #[test]
    fn test_base_action_compatibility() {
        let action = test_base_action();
        assert_bincode_compatible(&action, "BaseAction");
        assert_bincode_deserialize_compatible(&action, "BaseAction");
    }

    #[test]
    fn test_commit_type_standalone_compatibility() {
        let commit = CommitType::Standalone(vec![test_committed_account()]);
        assert_bincode_compatible(&commit, "CommitType::Standalone");
        assert_bincode_deserialize_compatible(
            &commit,
            "CommitType::Standalone",
        );
    }

    #[test]
    fn test_commit_type_with_base_actions_compatibility() {
        let commit = CommitType::WithBaseActions {
            committed_accounts: vec![test_committed_account()],
            base_actions: vec![test_base_action()],
        };
        assert_bincode_compatible(&commit, "CommitType::WithBaseActions");
        assert_bincode_deserialize_compatible(
            &commit,
            "CommitType::WithBaseActions",
        );
    }

    #[test]
    fn test_undelegate_type_standalone_compatibility() {
        let undelegate = UndelegateType::Standalone;
        assert_bincode_compatible(&undelegate, "UndelegateType::Standalone");
        assert_bincode_deserialize_compatible(
            &undelegate,
            "UndelegateType::Standalone",
        );
    }

    #[test]
    fn test_undelegate_type_with_base_actions_compatibility() {
        let undelegate =
            UndelegateType::WithBaseActions(vec![test_base_action()]);
        assert_bincode_compatible(
            &undelegate,
            "UndelegateType::WithBaseActions",
        );
        assert_bincode_deserialize_compatible(
            &undelegate,
            "UndelegateType::WithBaseActions",
        );
    }

    #[test]
    fn test_commit_and_undelegate_compatibility() {
        let cau = CommitAndUndelegate {
            commit_action: CommitType::Standalone(vec![
                test_committed_account(),
            ]),
            undelegate_action: UndelegateType::Standalone,
        };
        assert_bincode_compatible(&cau, "CommitAndUndelegate");
        assert_bincode_deserialize_compatible(&cau, "CommitAndUndelegate");
    }

    #[test]
    fn test_magic_base_intent_base_actions_compatibility() {
        let intent = MagicBaseIntent::BaseActions(vec![test_base_action()]);
        assert_bincode_compatible(&intent, "MagicBaseIntent::BaseActions");
        assert_bincode_deserialize_compatible(
            &intent,
            "MagicBaseIntent::BaseActions",
        );
    }

    #[test]
    fn test_magic_base_intent_commit_compatibility() {
        let intent = MagicBaseIntent::Commit(CommitType::Standalone(vec![
            test_committed_account(),
        ]));
        assert_bincode_compatible(&intent, "MagicBaseIntent::Commit");
        assert_bincode_deserialize_compatible(
            &intent,
            "MagicBaseIntent::Commit",
        );
    }

    #[test]
    fn test_magic_base_intent_commit_and_undelegate_compatibility() {
        let intent =
            MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
                commit_action: CommitType::Standalone(vec![
                    test_committed_account(),
                ]),
                undelegate_action: UndelegateType::WithBaseActions(vec![
                    test_base_action(),
                ]),
            });
        assert_bincode_compatible(
            &intent,
            "MagicBaseIntent::CommitAndUndelegate",
        );
        assert_bincode_deserialize_compatible(
            &intent,
            "MagicBaseIntent::CommitAndUndelegate",
        );
    }

    #[test]
    fn test_magic_intent_bundle_empty_compatibility() {
        let bundle = MagicIntentBundle::default();
        assert_bincode_compatible(&bundle, "MagicIntentBundle::default");
        assert_bincode_deserialize_compatible(
            &bundle,
            "MagicIntentBundle::default",
        );
    }

    #[test]
    fn test_magic_intent_bundle_full_compatibility() {
        let bundle = MagicIntentBundle {
            commit: Some(CommitType::Standalone(
                vec![test_committed_account()],
            )),
            commit_and_undelegate: Some(CommitAndUndelegate {
                commit_action: CommitType::WithBaseActions {
                    committed_accounts: vec![test_committed_account()],
                    base_actions: vec![test_base_action()],
                },
                undelegate_action: UndelegateType::Standalone,
            }),
            standalone_actions: vec![test_base_action()],
        };
        assert_bincode_compatible(&bundle, "MagicIntentBundle full");
        assert_bincode_deserialize_compatible(
            &bundle,
            "MagicIntentBundle full",
        );
    }

    #[test]
    fn test_scheduled_intent_bundle_compatibility() {
        let bundle = ScheduledIntentBundle {
            id: 12345,
            slot: 67890,
            blockhash: test_hash(),
            sent_transaction: Transaction::default(),
            payer: test_pubkey(),
            intent_bundle: MagicIntentBundle {
                commit: Some(CommitType::Standalone(vec![
                    test_committed_account(),
                ])),
                commit_and_undelegate: None,
                standalone_actions: vec![],
            },
        };
        assert_bincode_compatible(&bundle, "ScheduledIntentBundle");
        assert_bincode_deserialize_compatible(&bundle, "ScheduledIntentBundle");
    }

    #[test]
    fn test_magic_context_with_scheduled_intents_compatibility() {
        let context = MagicContext {
            intent_id: 999,
            scheduled_base_intents: vec![ScheduledIntentBundle {
                id: 1,
                slot: 100,
                blockhash: test_hash(),
                sent_transaction: Transaction::default(),
                payer: test_pubkey(),
                intent_bundle: MagicIntentBundle {
                    commit: Some(CommitType::Standalone(vec![
                        test_committed_account(),
                    ])),
                    commit_and_undelegate: Some(CommitAndUndelegate {
                        commit_action: CommitType::Standalone(vec![
                            test_committed_account(),
                        ]),
                        undelegate_action: UndelegateType::Standalone,
                    }),
                    standalone_actions: vec![test_base_action()],
                },
            }],
        };
        assert_bincode_compatible(
            &context,
            "MagicContext with scheduled intents",
        );
        assert_bincode_deserialize_compatible(
            &context,
            "MagicContext with scheduled intents",
        );
    }

    /// Test that bincode 2 can deserialize data that was serialized with bincode 1
    /// This is crucial for backwards compatibility with existing deployed contracts
    #[test]
    fn test_backwards_compatibility_magic_context() {
        let context = MagicContext {
            intent_id: 42,
            scheduled_base_intents: vec![ScheduledIntentBundle {
                id: 1,
                slot: 100,
                blockhash: test_hash(),
                sent_transaction: Transaction::default(),
                payer: test_pubkey(),
                intent_bundle: MagicIntentBundle::default(),
            }],
        };

        // Simulate data serialized by old bincode-based contracts
        let bincode1_serialized = bincode1::serialize(&context).unwrap();

        // Verify bincode 2 can deserialize it
        let deserialized: MagicContext = bincode::serde::decode_from_slice(
            &bincode1_serialized,
            bincode::config::legacy(),
        )
        .unwrap()
        .0;
        assert_eq!(deserialized.intent_id, context.intent_id);
        assert_eq!(
            deserialized.scheduled_base_intents.len(),
            context.scheduled_base_intents.len()
        );
    }

    /// Test that data serialized with bincode 2 can be deserialized with bincode 1
    /// This ensures new code is compatible with any remaining bincode 1 users
    #[test]
    fn test_forwards_compatibility_magic_context() {
        let context = MagicContext {
            intent_id: 123,
            scheduled_base_intents: vec![],
        };

        // Serialize with bincode 2 (new code)
        let bincode2_serialized =
            bincode::serde::encode_to_vec(&context, bincode::config::legacy())
                .unwrap();

        // Verify bincode 1 can deserialize it (old code compatibility)
        let deserialized: MagicContext =
            bincode1::deserialize(&bincode2_serialized).unwrap();
        assert_eq!(deserialized.intent_id, context.intent_id);
        assert!(deserialized.scheduled_base_intents.is_empty());
    }
}
