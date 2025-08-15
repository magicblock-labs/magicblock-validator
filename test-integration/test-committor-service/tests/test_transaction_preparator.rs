use std::collections::HashMap;

use magicblock_committor_service::{
    persist::IntentPersisterImpl,
    transaction_preparator::transaction_preparator::TransactionPreparator,
};
use magicblock_program::magic_scheduled_base_intent::{
    BaseAction, CommitAndUndelegate, CommitType, CommittedAccountV2,
    MagicBaseIntent, ProgramArgs, ScheduledBaseIntent, ShortAccountMeta,
    UndelegateType,
};
use solana_pubkey::Pubkey;
use solana_sdk::{
    account::Account, hash::Hash, signer::Signer, system_program,
    transaction::Transaction,
};

use crate::common::{create_committed_account, TestFixture};

mod common;

#[tokio::test]
async fn test_prepare_commit_tx_with_single_account() {
    let fixture = TestFixture::new().await;
    let preparator = fixture.create_transaction_preparator();

    // Create test data
    let account_data = vec![1, 2, 3, 4, 5];
    let committed_account = create_committed_account(&account_data);
    let l1_message = ScheduledBaseIntent {
        id: 1,
        slot: 0,
        blockhash: Hash::default(),
        action_sent_transaction: Transaction::default(),
        payer: fixture.authority.pubkey(),
        base_intent: MagicBaseIntent::Commit(CommitType::Standalone(vec![
            committed_account.clone(),
        ])),
    };

    let mut commit_ids = HashMap::new();
    commit_ids.insert(committed_account.pubkey, 1);

    // Test preparation
    let result = preparator
        .prepare_commit_tx(
            &fixture.authority,
            &l1_message,
            &None::<IntentPersisterImpl>,
        )
        .await;

    assert!(result.is_ok(), "Preparation failed: {:?}", result.err());
}

#[tokio::test]
async fn test_prepare_commit_tx_with_multiple_accounts() {
    let fixture = TestFixture::new().await;
    let preparator = fixture.create_transaction_preparator();

    // Create test data
    let accounts = vec![
        CommittedAccountV2 {
            pubkey: Pubkey::new_unique(),
            account: Account {
                lamports: 1000,
                data: vec![1, 2, 3],
                owner: system_program::id(),
                executable: false,
                rent_epoch: 0,
            },
        },
        CommittedAccountV2 {
            pubkey: Pubkey::new_unique(),
            account: Account {
                lamports: 2000,
                data: vec![4, 5, 6],
                owner: system_program::id(),
                executable: false,
                rent_epoch: 0,
            },
        },
    ];

    let l1_message = ScheduledBaseIntent {
        id: 1,
        slot: 0,
        blockhash: Hash::default(),
        action_sent_transaction: Transaction::default(),
        payer: fixture.authority.pubkey(),
        base_intent: MagicBaseIntent::Commit(CommitType::Standalone(
            accounts.clone(),
        )),
    };

    // Test preparation
    let result = preparator
        .prepare_commit_tx(
            &fixture.authority,
            &l1_message,
            &None::<IntentPersisterImpl>,
        )
        .await;

    assert!(result.is_ok(), "Preparation failed: {:?}", result.err());
}

#[tokio::test]
async fn test_prepare_commit_tx_with_l1_actions() {
    let fixture = TestFixture::new().await;
    let preparator = fixture.create_transaction_preparator();

    // Create test data
    let account = CommittedAccountV2 {
        pubkey: Pubkey::new_unique(),
        account: Account {
            lamports: 1000,
            data: vec![1, 2, 3],
            owner: system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    };

    let l1_action = BaseAction {
        compute_units: 30_000,
        destination_program: system_program::id(),
        escrow_authority: fixture.authority.pubkey(),
        data_per_program: ProgramArgs {
            escrow_index: 0,
            data: vec![4, 5, 6],
        },
        account_metas_per_program: vec![ShortAccountMeta {
            pubkey: Pubkey::new_unique(),
            is_writable: true,
        }],
    };

    let l1_message = ScheduledBaseIntent {
        id: 1,
        slot: 0,
        blockhash: Hash::default(),
        action_sent_transaction: Transaction::default(),
        payer: fixture.authority.pubkey(),
        base_intent: MagicBaseIntent::Commit(CommitType::WithBaseActions {
            committed_accounts: vec![account.clone()],
            base_actions: vec![l1_action],
        }),
    };

    let mut commit_ids = HashMap::new();
    commit_ids.insert(account.pubkey, 1);

    // Test preparation
    let result = preparator
        .prepare_commit_tx(
            &fixture.authority,
            &l1_message,
            &None::<IntentPersisterImpl>,
        )
        .await;

    assert!(result.is_ok(), "Preparation failed: {:?}", result.err());
}

#[tokio::test]
async fn test_prepare_finalize_tx_with_undelegate() {
    let fixture = TestFixture::new().await;
    let preparator = fixture.create_transaction_preparator();

    // Create test data
    let l1_message = ScheduledBaseIntent {
        id: 1,
        slot: 0,
        blockhash: Hash::default(),
        action_sent_transaction: Transaction::default(),
        payer: fixture.authority.pubkey(),
        base_intent: MagicBaseIntent::CommitAndUndelegate(
            CommitAndUndelegate {
                commit_action: CommitType::Standalone(vec![]),
                undelegate_action: UndelegateType::Standalone,
            },
        ),
    };

    // Test preparation
    let result = preparator
        .prepare_finalize_tx(
            &fixture.authority,
            &l1_message,
            &None::<IntentPersisterImpl>,
        )
        .await;

    assert!(result.is_ok(), "Preparation failed: {:?}", result.err());
}

#[tokio::test]
async fn test_prepare_finalize_tx_with_undelegate_and_actions() {
    let fixture = TestFixture::new().await;
    let preparator = fixture.create_transaction_preparator();

    // Create test data
    let l1_action = BaseAction {
        compute_units: 30_000,
        destination_program: system_program::id(),
        escrow_authority: fixture.authority.pubkey(),
        data_per_program: ProgramArgs {
            escrow_index: 0,
            data: vec![4, 5, 6],
        },
        account_metas_per_program: vec![ShortAccountMeta {
            pubkey: Pubkey::new_unique(),
            is_writable: true,
        }],
    };

    let l1_message = ScheduledBaseIntent {
        id: 1,
        slot: 0,
        blockhash: Hash::default(),
        action_sent_transaction: Transaction::default(),
        payer: fixture.authority.pubkey(),
        base_intent: MagicBaseIntent::CommitAndUndelegate(
            CommitAndUndelegate {
                commit_action: CommitType::Standalone(vec![]),
                undelegate_action: UndelegateType::WithBaseActions(vec![
                    l1_action,
                ]),
            },
        ),
    };

    // Test preparation
    let result = preparator
        .prepare_finalize_tx(
            &fixture.authority,
            &l1_message,
            &None::<IntentPersisterImpl>,
        )
        .await;

    assert!(result.is_ok(), "Preparation failed: {:?}", result.err());
}

#[tokio::test]
async fn test_prepare_large_commit_tx_uses_buffers() {
    let fixture = TestFixture::new().await;
    let preparator = fixture.create_transaction_preparator();

    // Create large account data (10KB)
    let account_data = vec![0; u16::MAX as usize + 1];
    let committed_account = CommittedAccountV2 {
        pubkey: Pubkey::new_unique(),
        account: Account {
            lamports: 1000,
            data: account_data,
            owner: system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    };

    let l1_message = ScheduledBaseIntent {
        id: 1,
        slot: 0,
        blockhash: Hash::default(),
        action_sent_transaction: Transaction::default(),
        payer: fixture.authority.pubkey(),
        base_intent: MagicBaseIntent::Commit(CommitType::Standalone(vec![
            committed_account.clone(),
        ])),
    };

    let mut commit_ids = HashMap::new();
    commit_ids.insert(committed_account.pubkey, 1);

    // Test preparation
    let result = preparator
        .prepare_commit_tx(
            &fixture.authority,
            &l1_message,
            &None::<IntentPersisterImpl>,
        )
        .await;

    assert!(result.is_ok(), "Preparation failed: {:?}", result.err());
}
