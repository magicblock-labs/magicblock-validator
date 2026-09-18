use integration_test_tools::{init_logger, run_test};
use program_schedulecommit::{
    api::{
        increase_count_instruction, init_order_book_instruction,
        schedule_commit_with_vault_and_order_book_instruction, UserSeeds,
    },
    MainAccount, ScheduleCommitWithOrderBookArgs,
};
use schedulecommit_client::{verify, ScheduleCommitTestContext};
use serial_test::serial;
use solana_rpc_client::rpc_client::SerializableTransaction;
use solana_rpc_client_api::config::RpcSendTransactionConfig;
use solana_sdk::{pubkey::Pubkey, signature::Signer, transaction::Transaction};
use tracing::info;

#[test]
#[serial]
fn commit_bundle_runs_post_commit_action() {
    run_test!({
        let ctx = ScheduleCommitTestContext::try_new_random_keys(
            1,
            UserSeeds::MagicScheduleCommit,
        )
        .expect("create scheduled-intent context");
        ctx.init_committees().expect("initialize committee");
        ctx.escrow_lamports_for_payer()
            .expect("initialize payer escrow");
        ctx.delegate_committees().expect("delegate committee");
        ctx.wait_for_next_slot_ephem()
            .expect("wait for delegated account");

        let fields = ctx.fields();
        let committee = &fields.committees[0];
        let increase = increase_count_instruction(committee.1);
        ctx.send_and_confirm_instructions_with_payer_ephem(
            &[increase],
            fields.payer_ephem,
        )
        .expect("increment delegated state");

        let (order_book, _) = Pubkey::find_program_address(
            &[b"order_book", fields.payer_chain.pubkey().as_ref()],
            &program_schedulecommit::id(),
        );
        let init_order_book = init_order_book_instruction(
            fields.payer_chain.pubkey(),
            fields.payer_chain.pubkey(),
            order_book,
        );
        ctx.send_and_confirm_instructions_with_payer_chain(
            &[init_order_book],
            fields.payer_chain,
        )
        .expect("initialize action target");

        let ix = schedule_commit_with_vault_and_order_book_instruction(
            fields.payer_ephem.pubkey(),
            *fields.validator_identity,
            magicblock_magic_program_api::id(),
            magicblock_magic_program_api::MAGIC_CONTEXT_PUBKEY,
            order_book,
            &[committee.1],
            ScheduleCommitWithOrderBookArgs {
                players: vec![committee.0.pubkey()],
                with_actions: true,
            },
        );
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&fields.payer_ephem.pubkey()),
            &[fields.payer_ephem],
            fields
                .ephem_client
                .get_latest_blockhash()
                .expect("fetch ephemeral blockhash"),
        );
        let signature = *tx.get_signature();
        fields
            .ephem_client
            .send_and_confirm_transaction_with_spinner_and_config(
                &tx,
                *fields.commitment,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..Default::default()
                },
            )
            .expect("schedule intent bundle");

        let result =
            verify::fetch_and_verify_commit_result_from_logs(&ctx, signature);
        result
            .confirm_commit_transactions_on_chain(&ctx)
            .expect("confirm intent on base layer");

        let committed = ctx
            .fetch_chain_account_struct::<MainAccount>(committee.1)
            .expect("read committed state");
        assert_eq!(committed.count, 1);

        let action_target = ctx
            .fetch_chain_account(order_book)
            .expect("read post-commit action target");
        assert!(
            action_target.data.iter().any(|byte| *byte != 0),
            "post-commit action must update the order book"
        );
    });
}
