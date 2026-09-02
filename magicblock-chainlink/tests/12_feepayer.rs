use dlp_api::pda::ephemeral_balance_pda_from_payer;
use engine::IntoTransactionView;
use magicblock_chainlink::{
    AccountFetchEntrypoint, assert_cloned_as_undelegated, assert_not_cloned,
    assert_not_subscribed, assert_subscribed,
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

/// Proves transaction ensure processes only static transaction keys and never
/// derives or materializes the retired fee-payer balance PDA.
#[tokio::test]
async fn transaction_ensure_ignores_ephemeral_balance_pda() {
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
            owner: dlp_api::id(),
            ..Default::default()
        },
    );
    let record = add_delegation_record_for(
        &ctx.rpc_client,
        balance,
        ctx.validator_pubkey,
        solana_sdk_ids::system_program::id(),
    );
    let transaction = fee_payer_transaction(&ctx, &payer);

    let claims = ctx
        .chainlink
        .ensure_accounts(
            transaction.static_account_keys(),
            AccountFetchEntrypoint::SendTransaction(
                transaction.signatures()[0],
            ),
        )
        .await
        .unwrap();

    assert_eq!(claims, 1);
    assert_cloned_as_undelegated!(ctx.bank, &[payer.pubkey()]);
    assert_not_cloned!(ctx.bank, &[balance, record]);
    assert_subscribed!(ctx.chainlink, &[&payer.pubkey()]);
    assert_not_subscribed!(ctx.chainlink, &[&balance, &record]);
}
