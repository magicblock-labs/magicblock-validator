use assert_matches::assert_matches;
use dlp_api::pda::delegation_record_pda_from_delegated_account;
use magicblock_chainlink::{
    AccountFetchContext, assert_cloned_as_delegated,
    assert_cloned_as_undelegated, assert_not_cloned, assert_not_subscribed,
    assert_not_undelegating, assert_remain_undelegating,
    assert_subscribed_without_delegation_record,
    testing::{context::TestContext, deleg::add_delegation_record_for},
};
use solana_account::{
    Account, AccountFieldPatch, AccountMode, AccountSharedData,
};
use solana_pubkey::Pubkey;
const CURRENT_SLOT: u64 = 11;

#[tokio::test]
async fn ensure_account_scenarios() {
    let ctx = TestContext::init(CURRENT_SLOT).await;
    write_non_existing_account(&ctx).await;
    existing_account_undelegated(&ctx).await;
    existing_account_missing_delegation_record(&ctx).await;
    write_existing_account_valid_delegation_record(&ctx).await;
    write_existing_account_other_authority(&ctx).await;
    write_undelegating_account_undelegated_to_other_validator(&ctx).await;
    write_undelegating_account_still_being_undelegated(&ctx).await;
    write_existing_account_invalid_delegation_record(&ctx).await;
}

// NOTE: Case comments refer to the case studies in the relevant tabs of draw.io document, i.e. Fetch

// -----------------
// Account does not exist
// -----------------
async fn write_non_existing_account(ctx: &TestContext) {
    let chainlink = &ctx.chainlink;
    let bank = &ctx.bank;

    let pubkey = Pubkey::new_unique();
    let pubkeys = [pubkey];
    chainlink
        .ensure_accounts(
            &pubkeys,
            None,
            AccountFetchContext::rpc_get_multiple_accounts(),
        )
        .await
        .unwrap();

    assert_not_cloned!(bank, &pubkeys);
    assert_not_subscribed!(chainlink, &[&pubkey]);
}

// -----------------
// BasicScenarios:Case 1 Account is initialized and never delegated
// -----------------
async fn existing_account_undelegated(ctx: &TestContext) {
    let chainlink = &ctx.chainlink;
    let rpc_client = &ctx.rpc_client;
    let bank = &ctx.bank;

    let pubkey = Pubkey::new_unique();
    rpc_client.add_account(pubkey, Account::default());

    let pubkeys = [pubkey];
    chainlink
        .ensure_accounts(
            &pubkeys,
            None,
            AccountFetchContext::rpc_get_multiple_accounts(),
        )
        .await
        .unwrap();

    assert_cloned_as_undelegated!(bank, &pubkeys, CURRENT_SLOT);
    assert_subscribed_without_delegation_record!(chainlink, &[&pubkey]);
}

// -----------------
// Failure cases account with missing/invalid delegation record
// -----------------
async fn existing_account_missing_delegation_record(ctx: &TestContext) {
    let chainlink = &ctx.chainlink;
    let rpc_client = &ctx.rpc_client;
    let bank = &ctx.bank;

    let pubkey = Pubkey::new_unique();
    rpc_client.add_account(
        pubkey,
        Account {
            owner: dlp_api::id(),
            ..Default::default()
        },
    );

    let pubkeys = [pubkey];
    chainlink
        .ensure_accounts(
            &pubkeys,
            None,
            AccountFetchContext::rpc_get_multiple_accounts(),
        )
        .await
        .unwrap();

    assert_cloned_as_undelegated!(bank, &pubkeys, CURRENT_SLOT);
    assert_subscribed_without_delegation_record!(chainlink, &[&pubkey]);
}

// -----------------
// BasicScenarios:Case 2 Account is initialized and already delegated to us
// -----------------
async fn write_existing_account_valid_delegation_record(ctx: &TestContext) {
    let chainlink = &ctx.chainlink;
    let rpc_client = &ctx.rpc_client;
    let bank = &ctx.bank;
    let validator_pubkey = ctx.validator_pubkey;

    let pubkey = Pubkey::new_unique();
    let owner = Pubkey::new_unique();

    let acc = Account {
        owner: dlp_api::id(),
        lamports: 1_000_000,
        ..Default::default()
    };
    rpc_client.add_account(pubkey, acc);

    let deleg_record_pubkey =
        add_delegation_record_for(rpc_client, pubkey, validator_pubkey, owner);

    let pubkeys = [pubkey];
    chainlink
        .ensure_accounts(
            &pubkeys,
            None,
            AccountFetchContext::rpc_get_multiple_accounts(),
        )
        .await
        .unwrap();

    // The account is cloned into the bank as delegated, the delegation record isn't
    assert_cloned_as_delegated!(bank, &[pubkey], CURRENT_SLOT, owner);
    assert_not_cloned!(bank, &[deleg_record_pubkey]);

    assert_not_subscribed!(
        chainlink,
        &[&deleg_record_pubkey, &validator_pubkey]
    );
}

// -----------------
// BasicScenarios:Case 3: Account Initialized and Already Delegated to Other
// -----------------
async fn write_existing_account_other_authority(ctx: &TestContext) {
    let chainlink = &ctx.chainlink;
    let rpc_client = &ctx.rpc_client;
    let bank = &ctx.bank;

    let pubkey = Pubkey::new_unique();
    let account = Account {
        owner: dlp_api::id(),
        ..Default::default()
    };
    rpc_client.add_account(pubkey, account);

    let owner = Pubkey::new_unique();
    let authority = Pubkey::new_unique();
    let deleg_record_pubkey =
        add_delegation_record_for(rpc_client, pubkey, authority, owner);

    let pubkeys = [pubkey];
    chainlink
        .ensure_accounts(
            &pubkeys,
            None,
            AccountFetchContext::rpc_get_multiple_accounts(),
        )
        .await
        .unwrap();

    // The account is cloned into the bank as undelegated, the delegation record isn't
    assert_cloned_as_undelegated!(bank, &pubkeys, CURRENT_SLOT, owner);
    assert_not_cloned!(bank, &[deleg_record_pubkey]);

    assert_subscribed_without_delegation_record!(chainlink, &[&pubkey]);
}

// -----------------
// Account is in the process of being undelegated and its owner is the delegation program
// -----------------
async fn write_undelegating_account_undelegated_to_other_validator(
    ctx: &TestContext,
) {
    let chainlink = &ctx.chainlink;
    let rpc_client = &ctx.rpc_client;
    let bank = &ctx.bank;

    let other_authority = Pubkey::new_unique();
    let pubkey = Pubkey::new_unique();

    // The account was re-delegated to other validator on chain
    let account = Account {
        owner: dlp_api::id(),
        ..Default::default()
    };
    let owner = Pubkey::new_unique();
    rpc_client.add_account(pubkey, account);

    add_delegation_record_for(rpc_client, pubkey, other_authority, owner);

    // The same account is already marked as undelegated in the bank
    // (set the owner to the delegation program and mark it as _undelegating_)
    let mut shared_data = AccountSharedData::from(Account {
        owner: dlp_api::id(),
        data: vec![0; 100],
        ..Default::default()
    });
    shared_data.set_mode(AccountMode::Transient);
    AccountFieldPatch::Slot(CURRENT_SLOT).apply(&mut shared_data);
    bank.account(pubkey)
        .create(shared_data.owned(), None)
        .await
        .unwrap();

    let pubkeys = [pubkey];
    chainlink
        .ensure_accounts(
            &pubkeys,
            None,
            AccountFetchContext::rpc_get_multiple_accounts(),
        )
        .await
        .unwrap();
    assert_not_undelegating!(bank, &pubkeys, CURRENT_SLOT);
}

async fn write_undelegating_account_still_being_undelegated(ctx: &TestContext) {
    let chainlink = &ctx.chainlink;
    let rpc_client = &ctx.rpc_client;
    let bank = &ctx.bank;
    let validator_pubkey = ctx.validator_pubkey;

    let authority = validator_pubkey;
    let pubkey = Pubkey::new_unique();

    // The account is still delegated to us on chain
    let account = Account {
        owner: dlp_api::id(),
        ..Default::default()
    };
    let owner = Pubkey::new_unique();
    rpc_client.add_account(pubkey, account);

    add_delegation_record_for(rpc_client, pubkey, authority, owner);

    // The same account is already marked as undelegated in the bank
    // (setting the owner to the delegation program marks it as _undelegating_)
    let mut shared_data = AccountSharedData::from(Account {
        owner: dlp_api::id(),
        data: vec![0; 100],
        ..Default::default()
    });
    AccountFieldPatch::Slot(CURRENT_SLOT).apply(&mut shared_data);
    shared_data.set_mode(AccountMode::Transient);
    bank.account(pubkey)
        .create(shared_data.owned(), None)
        .await
        .unwrap();

    let pubkeys = [pubkey];
    chainlink
        .ensure_accounts(
            &pubkeys,
            None,
            AccountFetchContext::rpc_get_multiple_accounts(),
        )
        .await
        .unwrap();
    assert_remain_undelegating!(bank, &pubkeys, CURRENT_SLOT);
}

// -----------------
// Invalid Cases
// -----------------
async fn write_existing_account_invalid_delegation_record(ctx: &TestContext) {
    let chainlink = &ctx.chainlink;
    let rpc_client = &ctx.rpc_client;
    let bank = &ctx.bank;

    let pubkey = Pubkey::new_unique();
    rpc_client.add_account(
        pubkey,
        Account {
            owner: dlp_api::id(),
            ..Default::default()
        },
    );
    let deleg_record_pubkey =
        delegation_record_pda_from_delegated_account(&pubkey);
    rpc_client.add_account(
        deleg_record_pubkey,
        Account {
            owner: dlp_api::id(),
            data: vec![1, 2, 3],
            ..Default::default()
        },
    );

    let res = chainlink
        .ensure_accounts(
            &[pubkey],
            None,
            AccountFetchContext::rpc_get_multiple_accounts(),
        )
        .await;

    assert_matches!(res, Err(_));
    assert!(bank.accounts().get(&pubkey).unwrap().is_none());

    assert_not_subscribed!(chainlink, &[&deleg_record_pubkey, &pubkey]);
}
