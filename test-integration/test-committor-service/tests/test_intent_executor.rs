use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use magicblock_committor_service::{
    intent_executor::{
        task_info_fetcher::{CacheTaskInfoFetcher, TaskInfoFetcher},
        ExecutionOutput, IntentExecutor, IntentExecutorImpl,
    },
    persist::{IntentPersister, IntentPersisterImpl},
    tasks::CommitTask,
    types::{ScheduledBaseIntentWrapper, TriggerType},
};
use magicblock_program::{
    magic_scheduled_base_intent::{
        CommitAndUndelegate, CommitType, CommittedAccount, MagicBaseIntent,
        ScheduledBaseIntent, UndelegateType,
    },
    validator::{
        generate_validator_authority_if_needed, init_validator_authority,
    },
};
use program_flexi_counter::delegation_program_id;
use solana_pubkey::Pubkey;
use solana_sdk::{hash::Hash, signature::Keypair, transaction::Transaction};

use crate::{
    common::{create_commit_task, generate_random_bytes, TestFixture},
    utils::{
        ensure_validator_authority,
        transactions::{
            fund_validator_auth_and_ensure_validator_fees_vault,
            init_and_delegate_account_on_chain,
        },
    },
};

mod common;
mod utils;

#[tokio::test]
async fn test_commit_id_error_recovery() {
    const COUNTER_SIZE: u64 = 100;

    let validator_auth = ensure_validator_authority();
    let fixture = TestFixture::new_with_keypair(validator_auth).await;
    fund_validator_auth_and_ensure_validator_fees_vault(&fixture.authority)
        .await;

    let transaction_preparator = fixture.create_transaction_preparator();
    let task_info_fetcher =
        Arc::new(CacheTaskInfoFetcher::new(fixture.rpc_client.clone()));

    let intent_executor = IntentExecutorImpl::new(
        fixture.rpc_client.clone(),
        transaction_preparator,
        task_info_fetcher.clone(),
    );

    let counter_auth = Keypair::new();
    let (pubkey, mut account) =
        init_and_delegate_account_on_chain(&counter_auth, COUNTER_SIZE).await;

    account.owner = program_flexi_counter::id();
    let committed_account = CommittedAccount { pubkey, account };
    let intent = create_intent(vec![committed_account.clone()], false);

    // Invalidate commit nonce cache
    let res = task_info_fetcher
        .fetch_next_commit_ids(&[committed_account.pubkey])
        .await;
    assert!(res.is_ok());
    assert!(res.unwrap().contains_key(&committed_account.pubkey));

    // Now execute intent
    let res = intent_executor
        .execute(intent, None::<IntentPersisterImpl>)
        .await;
    assert!(res.is_ok());
    assert!(matches!(res.unwrap(), ExecutionOutput::SingleStage(_)));
}

fn create_intent(
    committed_accounts: Vec<CommittedAccount>,
    is_undelegate: bool,
) -> ScheduledBaseIntent {
    static INTENT_ID: AtomicU64 = AtomicU64::new(0);

    let base_intent = if is_undelegate {
        MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
            commit_action: CommitType::Standalone(committed_accounts),
            undelegate_action: UndelegateType::Standalone,
        })
    } else {
        MagicBaseIntent::Commit(CommitType::Standalone(committed_accounts))
    };

    ScheduledBaseIntent {
        id: INTENT_ID.fetch_add(1, Ordering::Relaxed),
        slot: 10,
        blockhash: Hash::new_unique(),
        action_sent_transaction: Transaction::default(),
        payer: Pubkey::new_unique(),
        base_intent,
    }
}
