use dlp_api::{
    pda::delegation_record_pda_from_delegated_account, state::DelegationRecord,
};
use magicblock_chainlink::{
    AccountFetchContext, assert_cloned_as_delegated, assert_not_cloned,
    testing::{context::TestContext, deleg::delegation_record_to_vec},
};
use solana_account::Account;
use solana_pubkey::Pubkey;

const CURRENT_SLOT: u64 = 100;

#[tokio::test]
async fn resolves_delegated_account_from_mirrored_record_without_record_rpc() {
    let (ctx, mirror) =
        TestContext::init_with_record_mirror(CURRENT_SLOT).await;
    let pubkey = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    ctx.rpc_client.add_account(
        pubkey,
        Account {
            lamports: 1_000_000,
            owner: dlp_api::id(),
            ..Default::default()
        },
    );

    let record_pubkey = delegation_record_pda_from_delegated_account(&pubkey);
    mirror.test_insert_record(
        record_pubkey,
        delegation_record_to_vec(&DelegationRecord {
            authority: ctx.validator_pubkey,
            owner,
            delegation_slot: 1,
            lamports: 1_000,
            commit_frequency_ms: 2_000,
        }),
        CURRENT_SLOT,
    );
    mirror.test_set_watermark(CURRENT_SLOT);

    let calls_before = ctx.rpc_client.single_account_fetches()
        + ctx.rpc_client.multi_account_fetches();
    ctx.chainlink
        .ensure_accounts(
            &[pubkey],
            AccountFetchContext::rpc_get_multiple_accounts(),
        )
        .await
        .unwrap();

    assert_cloned_as_delegated!(ctx.bank, &[pubkey], CURRENT_SLOT, owner);
    assert_not_cloned!(ctx.bank, &[record_pubkey]);
    let calls = ctx.rpc_client.single_account_fetches()
        + ctx.rpc_client.multi_account_fetches()
        - calls_before;
    assert_eq!(calls, 1, "mirror resolution must not fetch the record");
}
