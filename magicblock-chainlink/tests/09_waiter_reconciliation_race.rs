use std::time::Duration;

use dlp_api::{
    args::{
        EncryptedBuffer, MaybeEncryptedAccountMeta, MaybeEncryptedInstruction,
        MaybeEncryptedIxData, PostDelegationActions,
    },
    pda::delegation_record_pda_from_delegated_account,
    state::DelegationRecord,
};
use magicblock_chainlink::{
    AccountFetchContext, assert_cloned_as_delegated, assert_not_subscribed,
    testing::{context::TestContext, deleg::delegation_record_to_vec},
};
use solana_account::{Account, AccountBuilder, AccountMode, ReadableAccount};
use solana_pubkey::Pubkey;
use v42_calculator_interface::{ID as V42_ID, builder::transfer};

const CURRENT_SLOT: u64 = 11;

fn add_increment_action(
    ctx: &TestContext,
    pubkey: Pubkey,
    output: Pubkey,
) -> Pubkey {
    let record_pubkey = delegation_record_pda_from_delegated_account(&pubkey);
    let record = DelegationRecord {
        authority: ctx.validator_pubkey,
        owner: V42_ID,
        delegation_slot: ctx.rpc_client.get_slot(),
        lamports: 1_000,
        commit_frequency_ms: 2_000,
    };
    let mut data = delegation_record_to_vec(&record);
    let action = transfer(pubkey, output, 1);
    let actions = PostDelegationActions {
        inserted_signers: 0,
        inserted_non_signers: 0,
        signers: vec![
            *pubkey.as_array(),
            *output.as_array(),
            *V42_ID.as_array(),
        ],
        non_signers: vec![],
        instructions: vec![MaybeEncryptedInstruction {
            program_id: 2,
            accounts: vec![
                MaybeEncryptedAccountMeta::ClearText(
                    dlp_api::compact::AccountMeta::new(0, false),
                ),
                MaybeEncryptedAccountMeta::ClearText(
                    dlp_api::compact::AccountMeta::new(1, false),
                ),
            ],
            data: MaybeEncryptedIxData {
                prefix: action.data,
                suffix: EncryptedBuffer::default(),
            },
        }],
    };
    data.extend_from_slice(&borsh::to_vec(&actions).unwrap());
    ctx.rpc_client.add_account(
        record_pubkey,
        Account {
            owner: dlp_api::id(),
            data,
            ..Default::default()
        },
    );
    record_pubkey
}

fn seed_output(ctx: &TestContext, output: Pubkey) {
    ctx.bank
        .accounts()
        .store(&[(
            output,
            AccountBuilder::default()
                .lamports(1_000_000)
                .data(0_i64.to_le_bytes().to_vec())
                .owner(V42_ID)
                .mode(AccountMode::Ephemeral)
                .build(),
        )])
        .unwrap();
}

fn account_value(ctx: &TestContext, pubkey: Pubkey) -> i64 {
    ctx.bank
        .accounts()
        .loader()
        .read(&pubkey, |account| {
            i64::from_le_bytes(account.data().try_into().unwrap())
        })
        .unwrap()
        .unwrap()
}

/// Proves a forced fetch racing the greedy-discovery subscription path submits
/// one account mutation and executes its post-delegation action exactly once.
#[tokio::test]
async fn fetch_and_discovery_subscription_race_materializes_once() {
    let ctx = TestContext::init(CURRENT_SLOT).await;
    let chainlink = ctx.chainlink.clone();
    let rpc_client = ctx.rpc_client.clone();
    let bank = ctx.bank.clone();

    let account_pubkey = Pubkey::new_unique();
    let output = Pubkey::new_unique();
    seed_output(&ctx, output);
    let remote_account = Account {
        lamports: 1_000_000,
        data: 10_i64.to_le_bytes().to_vec(),
        owner: dlp_api::id(),
        ..Default::default()
    };
    rpc_client.add_account(account_pubkey, remote_account.clone());
    let deleg_record_pubkey =
        add_increment_action(&ctx, account_pubkey, output);
    let mut updates = bank.accounts().subscribe(account_pubkey).await;
    let blocker = bank.account(account_pubkey).await;

    let requested = [account_pubkey];
    let ensure = chainlink.ensure_accounts(
        &requested,
        AccountFetchContext::rpc_get_multiple_accounts(),
    );
    let subscription = ctx.send_and_receive_account_update(
        account_pubkey,
        remote_account,
        Some(8_000),
    );
    let release = async move {
        tokio::task::yield_now().await;
        drop(blocker);
    };
    let (ensure_result, subscription_completed, ()) =
        tokio::join!(ensure, subscription, release);
    ensure_result.expect("ensure succeeds");
    assert!(subscription_completed, "subscription update completes");

    assert_cloned_as_delegated!(bank, &[account_pubkey], CURRENT_SLOT, V42_ID);
    let target_value = account_value(&ctx, account_pubkey);
    let output_value = account_value(&ctx, output);
    assert_eq!(
        (target_value, output_value),
        (9, 1),
        "post-delegation action executes exactly once"
    );
    updates.recv().await.expect("one materialization update");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), updates.recv())
            .await
            .is_err(),
        "race must not commit a second account mutation"
    );
    assert_not_subscribed!(chainlink, &[&account_pubkey, &deleg_record_pubkey]);
}

/// Proves a newer subscription-observed delegation replaces a transient bank
/// image and executes its post-delegation action exactly once.
#[tokio::test]
async fn transient_redelegation_subscription_executes_action_once() {
    let ctx = TestContext::init(CURRENT_SLOT).await;
    let pubkey = Pubkey::new_unique();
    let output = Pubkey::new_unique();
    seed_output(&ctx, output);
    ctx.bank
        .accounts()
        .store(&[(
            pubkey,
            AccountBuilder::default()
                .lamports(1_000_000)
                .data(10_i64.to_le_bytes().to_vec())
                .owner(dlp_api::id())
                .mode(AccountMode::Transient)
                .slot(CURRENT_SLOT)
                .build(),
        )])
        .unwrap();
    ctx.chainlink.undelegation_requested(pubkey).await.unwrap();

    let slot = ctx.rpc_client.set_slot(CURRENT_SLOT + 11);
    let remote = Account {
        lamports: 1_000_000,
        data: 10_i64.to_le_bytes().to_vec(),
        owner: dlp_api::id(),
        ..Default::default()
    };
    ctx.rpc_client.add_account(pubkey, remote.clone());
    let record = add_increment_action(&ctx, pubkey, output);

    assert!(
        ctx.send_and_receive_account_update(pubkey, remote, Some(8_000))
            .await,
        "subscription update completes"
    );
    assert_cloned_as_delegated!(ctx.bank, &[pubkey], slot, V42_ID);
    assert_eq!(
        (account_value(&ctx, pubkey), account_value(&ctx, output)),
        (9, 1),
        "redelegation action executes exactly once"
    );
    assert_not_subscribed!(ctx.chainlink, &[&pubkey, &record]);
}
