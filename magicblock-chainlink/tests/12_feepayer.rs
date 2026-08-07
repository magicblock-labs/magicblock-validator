use dlp_api::pda::{
    delegation_record_pda_from_delegated_account,
    ephemeral_balance_pda_from_payer,
};
use engine::IntoTransactionView;
use magicblock_chainlink::{
    assert_cloned_as_delegated, assert_cloned_as_empty_placeholder,
    assert_cloned_as_undelegated, assert_not_subscribed, assert_subscribed,
    testing::{
        context::TestContext, deleg::add_delegation_record_for, init_logger,
    },
};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction::Transaction;

fn fee_payer_transaction(
    ctx: &TestContext,
    payer: &Keypair,
) -> nucleus::runtime::TransactionView {
    let instruction = Instruction::new_with_bytes(
        solana_sdk_ids::system_program::id(),
        &[],
        vec![AccountMeta::new(payer.pubkey(), true)],
    );
    Transaction::new_signed_with_payer(
        &[instruction],
        Some(&payer.pubkey()),
        &[payer],
        ctx.bank.blockhash(),
    )
    .compose(&ctx.bank)
    .unwrap()
}

async fn setup_balance(delegated: bool) -> (TestContext, Keypair) {
    init_logger();
    let ctx = TestContext::init(100).await;
    let payer = Keypair::new();
    let balance = ephemeral_balance_pda_from_payer(&payer.pubkey(), 0);
    ctx.rpc_client.add_account(
        payer.pubkey(),
        Account {
            lamports: 2_000_000,
            ..Default::default()
        },
    );
    ctx.rpc_client.add_account(
        balance,
        Account {
            lamports: 1_000_000,
            owner: if delegated {
                dlp_api::id()
            } else {
                solana_sdk_ids::system_program::id()
            },
            ..Default::default()
        },
    );
    if delegated {
        add_delegation_record_for(
            &ctx.rpc_client,
            balance,
            ctx.validator_pubkey,
            solana_sdk_ids::system_program::id(),
        );
    }
    (ctx, payer)
}

#[tokio::test]
async fn fee_payer_uses_delegated_ephemeral_balance() {
    let (ctx, payer) = setup_balance(true).await;
    let balance = ephemeral_balance_pda_from_payer(&payer.pubkey(), 0);
    ctx.chainlink
        .ensure_transaction_accounts(&fee_payer_transaction(&ctx, &payer))
        .await
        .unwrap();

    assert_cloned_as_undelegated!(ctx.bank, &[payer.pubkey()]);
    assert_cloned_as_delegated!(ctx.bank, &[balance]);
    assert_subscribed!(ctx.chainlink, &[&payer.pubkey()]);
    assert_not_subscribed!(
        ctx.chainlink,
        &[
            &balance,
            &delegation_record_pda_from_delegated_account(&balance)
        ]
    );
}

#[tokio::test]
async fn fee_payer_uses_undelegated_ephemeral_balance() {
    let (ctx, payer) = setup_balance(false).await;
    let balance = ephemeral_balance_pda_from_payer(&payer.pubkey(), 0);
    ctx.chainlink
        .ensure_transaction_accounts(&fee_payer_transaction(&ctx, &payer))
        .await
        .unwrap();

    assert_cloned_as_undelegated!(ctx.bank, &[payer.pubkey(), balance]);
    assert_subscribed!(ctx.chainlink, &[&payer.pubkey(), &balance]);
}

#[tokio::test]
async fn fee_payer_without_ephemeral_balance_gets_placeholder() {
    init_logger();
    let ctx = TestContext::init(100).await;
    let payer = Keypair::new();
    let balance = ephemeral_balance_pda_from_payer(&payer.pubkey(), 0);
    ctx.rpc_client.add_account(
        payer.pubkey(),
        Account {
            lamports: 2_000_000,
            ..Default::default()
        },
    );

    ctx.chainlink
        .ensure_transaction_accounts(&fee_payer_transaction(&ctx, &payer))
        .await
        .unwrap();

    assert_cloned_as_undelegated!(ctx.bank, &[payer.pubkey()]);
    assert_cloned_as_empty_placeholder!(ctx.bank, &[balance]);
    assert_subscribed!(ctx.chainlink, &[&payer.pubkey(), &balance]);
}
