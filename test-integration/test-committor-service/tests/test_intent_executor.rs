use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    thread::sleep,
    time::Duration,
};

use borsh::to_vec;
use dlp::pda::ephemeral_balance_pda_from_payer;
use magicblock_committor_service::{
    intent_executor::{
        error::TransactionStrategyExecutionError,
        task_info_fetcher::{CacheTaskInfoFetcher, TaskInfoFetcher},
        ExecutionOutput, IntentExecutor, IntentExecutorImpl,
    },
    persist::IntentPersisterImpl,
    tasks::{
        task_builder::{TaskBuilderImpl, TasksBuilder},
        task_strategist::{TaskStrategist, TransactionStrategy},
    },
    transaction_preparator::TransactionPreparatorImpl,
};
use magicblock_program::{
    args::ShortAccountMeta,
    magic_scheduled_base_intent::{
        BaseAction, CommitAndUndelegate, CommitType, CommittedAccount,
        MagicBaseIntent, ProgramArgs, ScheduledBaseIntent, UndelegateType,
    },
};
use program_flexi_counter::{
    args::{CallHandlerDiscriminator, UndelegateActionData},
    state::FlexiCounter,
};
use solana_account::Account;
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    hash::Hash,
    native_token::LAMPORTS_PER_SOL,
    rent::Rent,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

use crate::{
    common::TestFixture,
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

const ACTOR_ESCROW_INDEX: u8 = 1;

struct TestEnv {
    fixture: TestFixture,
    task_info_fetcher: Arc<CacheTaskInfoFetcher>,
    intent_executor:
        IntentExecutorImpl<TransactionPreparatorImpl, CacheTaskInfoFetcher>,
}

impl TestEnv {
    async fn setup() -> Self {
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

        Self {
            fixture,
            task_info_fetcher,
            intent_executor,
        }
    }

    fn authority(&self) -> &Keypair {
        &self.fixture.authority
    }
}

#[tokio::test]
async fn test_commit_id_error_parsing() {
    const COUNTER_SIZE: u64 = 70;

    let TestEnv {
        fixture,
        intent_executor,
        task_info_fetcher,
    } = TestEnv::setup().await;
    let (counter_auth, account) = setup_counter(COUNTER_SIZE).await;
    let intent = create_intent(
        vec![CommittedAccount {
            pubkey: FlexiCounter::pda(&counter_auth.pubkey()).0,
            account,
        }],
        true,
    );

    // Invalidate ids before execution
    task_info_fetcher
        .fetch_next_commit_ids(&intent.get_committed_pubkeys().unwrap())
        .await
        .unwrap();

    let mut transaction_strategy = single_flow_transaction_strategy(
        &fixture.authority.pubkey(),
        &task_info_fetcher,
        &intent,
    )
    .await;
    let execution_result = intent_executor
        .prepare_and_execute_strategy(
            &mut transaction_strategy,
            &None::<IntentPersisterImpl>,
        )
        .await;
    assert!(execution_result.is_ok(), "Preparation is expected to pass!");

    // Verify that we got CommitIdError
    let execution_result = execution_result.unwrap();
    assert!(execution_result.is_err());
    assert!(matches!(
        execution_result.unwrap_err(),
        TransactionStrategyExecutionError::CommitIDError
    ))
}

#[tokio::test]
async fn test_action_error_parsing() {
    const COUNTER_SIZE: u64 = 70;

    let TestEnv {
        fixture,
        intent_executor,
        task_info_fetcher,
    } = TestEnv::setup().await;

    let (counter_auth, account) = setup_counter(COUNTER_SIZE).await;
    setup_payer_with_keypair(&counter_auth, fixture.rpc_client.get_inner())
        .await;

    let committed_account = CommittedAccount {
        pubkey: FlexiCounter::pda(&counter_auth.pubkey()).0,
        account,
    };

    // Create Intent with invalid action
    let commit_action = CommitType::Standalone(vec![committed_account.clone()]);
    let undelegate_action = failing_undelegate_action(
        counter_auth.pubkey(),
        committed_account.pubkey,
    );
    let base_intent =
        MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
            commit_action,
            undelegate_action,
        });

    let scheduled_intent = create_scheduled_intent(base_intent);
    let mut transaction_strategy = single_flow_transaction_strategy(
        &fixture.authority.pubkey(),
        &task_info_fetcher,
        &scheduled_intent,
    )
    .await;
    let execution_result = intent_executor
        .prepare_and_execute_strategy(
            &mut transaction_strategy,
            &None::<IntentPersisterImpl>,
        )
        .await;
    assert!(execution_result.is_ok(), "Preparation is expected to pass!");

    // Verify that we got CommitIdError
    let execution_result = execution_result.unwrap();
    assert!(execution_result.is_err());
    assert!(matches!(
        execution_result.unwrap_err(),
        TransactionStrategyExecutionError::ActionsError
    ))
}

#[tokio::test]
async fn test_commit_id_error_recovery() {
    const COUNTER_SIZE: u64 = 100;

    let TestEnv {
        fixture: _,
        intent_executor,
        task_info_fetcher,
    } = TestEnv::setup().await;

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

#[tokio::test]
async fn test_action_error_recovery() {
    const COUNTER_SIZE: u64 = 100;

    let TestEnv {
        fixture,
        intent_executor,
        task_info_fetcher: _,
    } = TestEnv::setup().await;

    let payer = setup_payer(fixture.rpc_client.get_inner()).await;
    let (counter_pubkey, mut account) =
        init_and_delegate_account_on_chain(&payer, COUNTER_SIZE).await;

    account.owner = program_flexi_counter::id();
    let committed_account = CommittedAccount {
        pubkey: counter_pubkey,
        account,
    };

    // Create Intent with invalid action
    let commit_action = CommitType::Standalone(vec![committed_account.clone()]);
    let undelegate_action =
        failing_undelegate_action(payer.pubkey(), committed_account.pubkey);
    let base_intent =
        MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
            commit_action,
            undelegate_action,
        });

    let scheduled_intent = create_scheduled_intent(base_intent);
    let res = intent_executor
        .execute(scheduled_intent, None::<IntentPersisterImpl>)
        .await;
    assert!(res.is_ok());
    assert!(matches!(res.unwrap(), ExecutionOutput::SingleStage(_)));
}

#[tokio::test]
async fn test_commit_id_and_action_errors_recovery() {
    const COUNTER_SIZE: u64 = 100;

    let TestEnv {
        fixture,
        intent_executor,
        task_info_fetcher,
    } = TestEnv::setup().await;

    let payer = setup_payer(fixture.rpc_client.get_inner()).await;
    let (counter_pubkey, mut account) =
        init_and_delegate_account_on_chain(&payer, COUNTER_SIZE).await;

    account.owner = program_flexi_counter::id();
    let committed_account = CommittedAccount {
        pubkey: counter_pubkey,
        account,
    };

    // Invalidate commit nonce cache
    let res = task_info_fetcher
        .fetch_next_commit_ids(&[committed_account.pubkey])
        .await;
    assert!(res.is_ok());
    assert!(res.unwrap().contains_key(&committed_account.pubkey));

    // Create Intent with invalid action
    let commit_action = CommitType::Standalone(vec![committed_account.clone()]);
    let undelegate_action =
        failing_undelegate_action(payer.pubkey(), committed_account.pubkey);
    let base_intent =
        MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
            commit_action,
            undelegate_action,
        });

    let scheduled_intent = create_scheduled_intent(base_intent);
    let res = intent_executor
        .execute(scheduled_intent, None::<IntentPersisterImpl>)
        .await;
    assert!(res.is_ok());
    assert!(matches!(res.unwrap(), ExecutionOutput::SingleStage(_)));
}

fn failing_undelegate_action(
    escrow_authority: Pubkey,
    undelegated_account: Pubkey,
) -> UndelegateType {
    const PRIZE: u64 = 1_000_000;
    const BREAKING_DIFF: i64 = -1000000; // Breaks action

    let undelegate_action_data = UndelegateActionData {
        counter_diff: BREAKING_DIFF,
        transfer_amount: PRIZE,
    };

    let transfer_destination = Pubkey::new_unique();
    let program_data = [
        CallHandlerDiscriminator::Simple.to_vec(),
        to_vec(&undelegate_action_data).unwrap(),
    ]
    .concat();

    let account_metas = vec![
        ShortAccountMeta {
            pubkey: undelegated_account,
            is_writable: true,
        },
        ShortAccountMeta {
            pubkey: transfer_destination,
            is_writable: true,
        },
        ShortAccountMeta {
            pubkey: solana_sdk::system_program::id(),
            is_writable: false,
        },
    ];

    UndelegateType::WithBaseActions(vec![BaseAction {
        compute_units: 100_000,
        destination_program: program_flexi_counter::id(),
        escrow_authority,
        data_per_program: ProgramArgs {
            escrow_index: ACTOR_ESCROW_INDEX,
            data: program_data,
        },
        account_metas_per_program: account_metas,
    }])
}

async fn setup_payer(rpc_client: &Arc<RpcClient>) -> Keypair {
    let payer = Keypair::new();
    setup_payer_with_keypair(&payer, rpc_client).await;

    payer
}

async fn setup_payer_with_keypair(
    payer: &Keypair,
    rpc_client: &Arc<RpcClient>,
) {
    let sig = rpc_client
        .request_airdrop(&payer.pubkey(), LAMPORTS_PER_SOL)
        .await
        .unwrap();
    rpc_client
        .confirm_transaction_with_commitment(
            &sig,
            CommitmentConfig::finalized(),
        )
        .await
        .unwrap();

    sleep(Duration::from_secs(1));
    // Create actor escrow
    let ix = dlp::instruction_builder::top_up_ephemeral_balance(
        payer.pubkey(),
        payer.pubkey(),
        Some(LAMPORTS_PER_SOL / 2),
        Some(ACTOR_ESCROW_INDEX),
    );
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&payer.pubkey()),
        &[&payer],
        rpc_client.get_latest_blockhash().await.unwrap(),
    );
    rpc_client.send_and_confirm_transaction(&tx).await.unwrap();

    // Confirm actor escrow
    let escrow_pda =
        ephemeral_balance_pda_from_payer(&payer.pubkey(), ACTOR_ESCROW_INDEX);
    let rent = Rent::default().minimum_balance(0);
    assert_eq!(
        rpc_client.get_account(&escrow_pda).await.unwrap().lamports,
        LAMPORTS_PER_SOL / 2 + rent
    );
}

async fn setup_counter(counter_bytes: u64) -> (Keypair, Account) {
    let counter_auth = Keypair::new();
    let (_, mut account) =
        init_and_delegate_account_on_chain(&counter_auth, counter_bytes).await;

    account.owner = program_flexi_counter::id();
    (counter_auth, account)
}

fn create_intent(
    committed_accounts: Vec<CommittedAccount>,
    is_undelegate: bool,
) -> ScheduledBaseIntent {
    let base_intent = if is_undelegate {
        MagicBaseIntent::CommitAndUndelegate(CommitAndUndelegate {
            commit_action: CommitType::Standalone(committed_accounts),
            undelegate_action: UndelegateType::Standalone,
        })
    } else {
        MagicBaseIntent::Commit(CommitType::Standalone(committed_accounts))
    };

    create_scheduled_intent(base_intent)
}

fn create_scheduled_intent(
    base_intent: MagicBaseIntent,
) -> ScheduledBaseIntent {
    static INTENT_ID: AtomicU64 = AtomicU64::new(0);

    ScheduledBaseIntent {
        id: INTENT_ID.fetch_add(1, Ordering::Relaxed),
        slot: 10,
        blockhash: Hash::new_unique(),
        action_sent_transaction: Transaction::default(),
        payer: Pubkey::new_unique(),
        base_intent,
    }
}

// TODO(edwin): for convinience need to be provided as api
async fn single_flow_transaction_strategy(
    authority: &Pubkey,
    task_info_fetcher: &Arc<CacheTaskInfoFetcher>,
    intent: &ScheduledBaseIntent,
) -> TransactionStrategy {
    let mut tasks = TaskBuilderImpl::commit_tasks(
        task_info_fetcher,
        &intent,
        &None::<IntentPersisterImpl>,
    )
    .await
    .unwrap();
    let finalize_tasks =
        TaskBuilderImpl::finalize_tasks(task_info_fetcher, &intent)
            .await
            .unwrap();
    tasks.extend(finalize_tasks);

    TaskStrategist::build_strategy(
        tasks,
        authority,
        &None::<IntentPersisterImpl>,
    )
    .unwrap()
}
