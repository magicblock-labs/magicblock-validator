use engine::IntoTransactionView;
use magicblock_chainlink::{
    assert_cloned_as_delegated, assert_cloned_as_undelegated,
    assert_not_subscribed, assert_subscribed,
    testing::{
        context::TestContext, deleg::add_delegation_record_for, init_logger,
    },
};
use solana_account::Account;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use v42_calculator_interface::ID as V42_ID;

#[tokio::test]
async fn resolves_mixed_transaction_accounts_in_one_request() {
    init_logger();
    let ctx = TestContext::init(100).await;
    let payer = Keypair::new();
    let writable = Pubkey::new_unique();
    let readonly = Pubkey::new_unique();
    let owner = Pubkey::new_unique();

    ctx.rpc_client.add_account(
        payer.pubkey(),
        Account {
            lamports: 1_000_000,
            owner,
            ..Default::default()
        },
    );
    ctx.rpc_client.add_account(
        writable,
        Account {
            lamports: 1_000_000,
            owner: dlp_api::id(),
            ..Default::default()
        },
    );
    add_delegation_record_for(
        &ctx.rpc_client,
        writable,
        ctx.validator_pubkey,
        owner,
    );
    ctx.rpc_client.add_account(
        readonly,
        Account {
            lamports: 1_000_000,
            owner,
            ..Default::default()
        },
    );

    let instruction = Instruction::new_with_bytes(
        V42_ID,
        &[],
        vec![
            AccountMeta::new(writable, false),
            AccountMeta::new_readonly(readonly, false),
        ],
    );
    let transaction = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&payer.pubkey()),
        &[&payer],
        ctx.bank.blockhash(),
    )
    .compose(&ctx.bank)
    .unwrap();

    ctx.chainlink
        .ensure_transaction_accounts(&transaction)
        .await
        .unwrap();

    assert_cloned_as_undelegated!(ctx.bank, &[payer.pubkey(), readonly]);
    assert_cloned_as_delegated!(ctx.bank, &[writable]);
    assert_subscribed!(ctx.chainlink, &[&payer.pubkey(), &readonly]);
    assert_not_subscribed!(ctx.chainlink, &[&writable, &V42_ID]);
}
