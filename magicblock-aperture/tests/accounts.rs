use std::collections::HashSet;

use setup::{PROGRAM_ID, RpcTestEnv, TOKEN_PROGRAM_ID};
use solana_account::{AccountMode, accounts_equal};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::request::TokenAccountsFilter;

mod setup;

/// Verifies `getAccountInfo` for both existing and non-existent accounts.
#[tokio::test]
async fn test_get_account_info() {
    let env = RpcTestEnv::new().await;

    // Test for an existing account
    let key = env.engine.store_v42(0, AccountMode::Ephemeral);
    let expected = env.engine.account(key).expect("stored account");
    let account = env
        .rpc
        .get_account(&key)
        .await
        .expect("failed to fetch created account");
    assert!(
        accounts_equal(&account, &expected),
        "created account doesn't match the rpc response"
    );

    // Test for a non-existent account
    let nonexistent = env
        .rpc
        .get_account_with_commitment(&Pubkey::new_unique(), Default::default())
        .await
        .expect("rpc request for non-existent account failed");
    assert_eq!(nonexistent.value, None, "account should not exist");

    // Repeated lookup should continue to render the synthetic empty placeholder
    // as JSON-RPC null after the ensure path has populated the bank.
    let missing_pubkey = Pubkey::new_unique();
    let first_miss = env
        .rpc
        .get_account_with_commitment(&missing_pubkey, Default::default())
        .await
        .expect("first rpc request for non-existent account failed");
    let second_miss = env
        .rpc
        .get_account_with_commitment(&missing_pubkey, Default::default())
        .await
        .expect("second rpc request for non-existent account failed");
    let latest_slot = env.engine.blocks().current_slot();
    assert!(
        first_miss.context.slot <= latest_slot,
        "first lookup context slot should not be ahead of the ledger: context={}, latest={latest_slot}",
        first_miss.context.slot
    );
    assert!(
        second_miss.context.slot >= first_miss.context.slot
            && second_miss.context.slot <= latest_slot,
        "second lookup context slot should be monotonic and not ahead of the ledger: first={}, second={}, latest={latest_slot}",
        first_miss.context.slot,
        second_miss.context.slot
    );
    assert_eq!(first_miss.value, None, "first lookup should return null");
    assert_eq!(
        second_miss.value, None,
        "repeated lookup should still return null"
    );
}

/// Verifies `getMultipleAccounts` for both existing and non-existent accounts.
#[tokio::test]
async fn test_get_multiple_accounts() {
    let env = RpcTestEnv::new().await;

    // Test with a list of existing accounts
    let acc1 = env.engine.store_v42(1, AccountMode::Ephemeral);
    let acc2 = env.engine.store_v42(2, AccountMode::Ephemeral);
    let accounts = env
        .rpc
        .get_multiple_accounts(&[acc1, acc2])
        .await
        .expect("failed to fetch newly created accounts");
    assert_eq!(accounts.len(), 2, "should return two accounts");
    assert!(
        accounts.iter().all(Option::is_some),
        "all existing accounts should be found"
    );

    // Test with a list of non-existent accounts
    let nonexistent = env
        .rpc
        .get_multiple_accounts(&[Pubkey::new_unique(), Pubkey::new_unique()])
        .await
        .expect("rpc request for non-existent accounts failed");
    assert!(
        nonexistent.iter().all(Option::is_none),
        "non-existent accounts should not be found"
    );

    // Mixed existing and non-existent accounts should preserve ordering and
    // still render synthetic empty placeholders as JSON-RPC null.
    let missing_pubkey = Pubkey::new_unique();
    let mixed = env
        .rpc
        .get_multiple_accounts(&[acc1, missing_pubkey, acc2])
        .await
        .expect(
            "rpc request for mixed existing and non-existent accounts failed",
        );
    assert_eq!(
        mixed.len(),
        3,
        "should return one entry per requested pubkey"
    );
    assert!(
        accounts_equal(
            mixed[0]
                .as_ref()
                .expect("existing first account should be returned"),
            env.engine.account(acc1).as_ref().expect("stored account")
        ),
        "first result should match the first requested account"
    );
    assert_eq!(
        mixed[1], None,
        "missing middle account should render as null"
    );
    assert!(
        accounts_equal(
            mixed[2]
                .as_ref()
                .expect("existing last account should be returned"),
            env.engine.account(acc2).as_ref().expect("stored account")
        ),
        "last result should match the last requested account"
    );
}

/// Verifies `getBalance` for both existing and non-existent accounts.
#[tokio::test]
async fn test_get_balance() {
    let env = RpcTestEnv::new().await;

    // Test balance of an existing account
    let acc = env.engine.store_v42(0, AccountMode::Ephemeral);
    let balance = env
        .rpc
        .get_balance(&acc)
        .await
        .expect("failed to fetch balance for newly created account");
    assert_eq!(
        balance,
        env.engine.load_v42_lamports(acc).expect("stored balance"),
        "rpc balance should match the account's lamports"
    );

    // Test balance of a non-existent account
    let balance = env
        .rpc
        .get_balance(&Pubkey::new_unique())
        .await
        .expect("failed to fetch balance for non-existent account");
    assert_eq!(
        balance, 0,
        "balance of a non-existent account should be zero"
    );
}

/// Verifies `getTokenAccountBalance` for both existing and non-existent token accounts.
#[tokio::test]
async fn test_get_token_account_balance() {
    let env = RpcTestEnv::new().await;
    let mint = Pubkey::new_unique();
    let owner = Pubkey::new_unique();

    // Test a valid token account
    let token_account = env.create_token_account(mint, owner);
    let balance = env
        .rpc
        .get_token_account_balance(&token_account)
        .await
        .expect("failed to fetch balance for newly created token account");
    assert_eq!(balance.decimals, 9, "balance decimals should be correct");
    assert_eq!(balance.amount, RpcTestEnv::TOKEN_AMOUNT.to_string());

    // Test a non-existent account, which should error.
    // This differs from `getBalance` which returns 0 for any pubkey.
    let nonexistent_result = env
        .rpc
        .get_token_account_balance(&Pubkey::new_unique())
        .await;
    assert!(
        nonexistent_result.is_err(),
        "fetching balance of a non-token account should result in an error"
    );
}

/// Verifies `getProgramAccounts` finds all accounts owned by a program.
#[tokio::test]
async fn test_get_program_accounts() {
    let env = RpcTestEnv::new().await;

    // Test a program with multiple accounts
    let acc1 = env.engine.store_v42(1, AccountMode::Ephemeral);
    let acc2 = env.engine.store_v42(2, AccountMode::Ephemeral);
    let expected_pubkeys: HashSet<Pubkey> = [acc1, acc2].into();

    let accounts = env
        .rpc
        .get_program_accounts(&PROGRAM_ID)
        .await
        .expect("failed to fetch accounts for program");

    assert_eq!(
        accounts.len(),
        2,
        "should return all accounts for the program"
    );
    for (pubkey, account) in accounts {
        assert!(expected_pubkeys.contains(&pubkey));
        assert_eq!(account.owner, PROGRAM_ID);
    }

    // Test a program with no accounts
    let empty_program_accounts = env
        .rpc
        .get_program_accounts(&Pubkey::new_unique())
        .await
        .unwrap();
    assert!(
        empty_program_accounts.is_empty(),
        "should return an empty list for a program with no accounts"
    );
}

/// Verifies `getTokenAccountsByOwner` using both Mint and ProgramId filters.
#[tokio::test]
async fn test_get_token_accounts_by_owner() {
    let env = RpcTestEnv::new().await;
    let mint = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let acc1 = env.create_token_account(mint, owner);
    let acc2 = env.create_token_account(mint, owner);

    let filters = [
        TokenAccountsFilter::Mint(mint),
        TokenAccountsFilter::ProgramId(TOKEN_PROGRAM_ID),
    ];

    for filter in filters {
        let accounts = env
            .rpc
            .get_token_accounts_by_owner(&owner, filter)
            .await
            .expect("failed to fetch token accounts by owner");

        assert_eq!(accounts.len(), 2, "should return two token accounts");
        assert!(accounts.iter().any(|a| a.pubkey == acc1.to_string()));
        assert!(accounts.iter().any(|a| a.pubkey == acc2.to_string()));
    }

    // Test with a non-existent mint
    let nonexistent = env
        .rpc
        .get_token_accounts_by_owner(
            &owner,
            TokenAccountsFilter::Mint(Pubkey::new_unique()),
        )
        .await
        .expect("RPC call for non-existent mint should not fail");
    assert!(
        nonexistent.is_empty(),
        "should return an empty list for a non-existent mint"
    );
}

/// Verifies `getTokenAccountsByDelegate` using both Mint and ProgramId filters.
#[tokio::test]
async fn test_get_token_accounts_by_delegate() {
    let env = RpcTestEnv::new().await;
    let mint = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    env.create_token_account(mint, owner);
    env.create_token_account(mint, owner);

    let filters = [
        TokenAccountsFilter::Mint(mint),
        TokenAccountsFilter::ProgramId(TOKEN_PROGRAM_ID),
    ];

    for filter in filters {
        let accounts = env
            .rpc
            .get_token_accounts_by_delegate(&owner, filter)
            .await
            .expect("failed to fetch token accounts by delegate");

        assert_eq!(
            accounts.len(),
            2,
            "should return two token accounts for the delegate"
        );
    }

    // Test with a non-existent program ID
    let nonexistent = env
        .rpc
        .get_token_accounts_by_delegate(
            &owner,
            TokenAccountsFilter::ProgramId(Pubkey::new_unique()),
        )
        .await
        .expect("RPC call for non-existent program should not fail");

    assert!(
        nonexistent.is_empty(),
        "should return an empty list for a non-existent program ID"
    );
}
