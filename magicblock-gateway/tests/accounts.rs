use setup::{RpcTestEnv, TOKEN_PROGRAM_ID};
use solana_account::{accounts_equal, ReadableAccount};
use solana_pubkey::Pubkey;
use solana_rpc_client_api::request::TokenAccountsFilter;

mod setup;

#[tokio::test]
async fn test_get_account_info() {
    let env = RpcTestEnv::new().await;
    let acc = env.create_account();
    let account = env
        .rpc
        .get_account(&acc.pubkey)
        .await
        .expect("failed to fetch created account");
    assert!(
        accounts_equal(&account, &acc.account),
        "created account doesn't match the rpc response"
    );
    let nonexistent = env
        .rpc
        .get_account_with_commitment(&Pubkey::new_unique(), Default::default())
        .await
        .expect("rpc request for non-existent account failed");
    assert_eq!(nonexistent.context.slot, env.execution.accountsdb.slot());
    assert_eq!(nonexistent.value, None, "account shouldn't have existed");
}

#[tokio::test]
async fn test_get_multiple_accounts() {
    let env = RpcTestEnv::new().await;
    let acc1 = env.create_account();
    let acc2 = env.create_account();
    let accounts = env
        .rpc
        .get_multiple_accounts(&[acc1.pubkey, acc2.pubkey])
        .await
        .expect("failed to fetch newly created accounts");
    assert!(
        !accounts.is_empty(),
        "gMA should return a non empty list of created accounts"
    );
    assert!(
        accounts.iter().all(Option::is_some),
        "all account should have been present in the database"
    );
    let nonexistent = env
        .rpc
        .get_multiple_accounts(&[Pubkey::new_unique(), Pubkey::new_unique()])
        .await
        .expect("rpc request for non-existent accounts failed");
    assert!(
        nonexistent.iter().all(Option::is_none),
        "none of the requested accounts should have been present in the database"
    );
}

#[tokio::test]
async fn test_get_balance() {
    let env = RpcTestEnv::new().await;
    let acc = env.create_account();
    let balance = env
        .rpc
        .get_balance(&acc.pubkey)
        .await
        .expect("failed to fetch balance for newly created account");
    assert_eq!(
        balance,
        acc.account.lamports(),
        "balance fetched from rpc should match the one from database"
    );
    let balance = env
        .rpc
        .get_balance(&Pubkey::new_unique())
        .await
        .expect("failed to fetch balance for nonexistent account");
    assert_eq!(
        balance, 0,
        "balance fetched from rpc for nonexistent account should be zero"
    );
}

#[tokio::test]
async fn test_get_token_account_balance() {
    let env = RpcTestEnv::new().await;
    let mint = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let token = env.create_token_account(mint, owner);
    let balance = env
        .rpc
        .get_token_account_balance(&token.pubkey)
        .await
        .expect("failed to fetch balance for newly created token account");
    assert_eq!(
        balance.decimals, 9,
        "balance fetched from rpc should match the one from database"
    );
    let nonexistent = env
        .rpc
        .get_token_account_balance(&Pubkey::new_unique())
        .await;
    assert!(
        nonexistent.is_err(),
        "fetching non existent token account's balance should result in error"
    );
}

#[tokio::test]
async fn test_get_program_accounts() {
    let env = RpcTestEnv::new().await;
    let acc1 = env.create_account();
    let acc2 = env.create_account();

    let accounts = env
        .rpc
        .get_program_accounts(acc1.account.owner())
        .await
        .expect("failed to fetch newly created accounts for program");
    assert!(
        !accounts.is_empty(),
        "gPA response should be a non-empty list of created accounts"
    );
    for (pubkey, account) in accounts {
        assert!(
            pubkey == acc1.pubkey || pubkey == acc2.pubkey,
            "getProgramAccounts returned irrelevant account"
        );
        assert_eq!(
            account.owner,
            *acc1.account.owner(),
            "owner mismatch in the result of getProgramAccounts"
        );
    }
}

#[tokio::test]
async fn test_get_token_accounts_by_filter() {
    let env = RpcTestEnv::new().await;
    let mint = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    let acc1 = env.create_token_account(mint, owner);
    let acc2 = env.create_token_account(mint, owner);

    for filter in [
        TokenAccountsFilter::Mint(mint),
        TokenAccountsFilter::ProgramId(TOKEN_PROGRAM_ID),
    ] {
        let accounts = env
            .rpc
            .get_token_accounts_by_owner(&owner, filter)
            .await
            .expect("failed to fetch newly created accounts for program");
        assert!(
            !accounts.is_empty(),
            "gTABO should return non empty list of accounts"
        );
        for account in accounts {
            let pubkey: Pubkey = account.pubkey.parse().unwrap();
            assert!(
                pubkey == acc1.pubkey || pubkey == acc2.pubkey,
                "getTokenAccountsByOwner returned irrelevant account"
            );
            assert_eq!(
                account.account.data.decode().unwrap().len(),
                165,
                "token account data length mismatch in the result of getTokenAccountsByOwner"
            );
        }
    }
    for filter in [
        TokenAccountsFilter::Mint(mint),
        TokenAccountsFilter::ProgramId(TOKEN_PROGRAM_ID),
    ] {
        let accounts = env
            .rpc
            .get_token_accounts_by_delegate(&owner, filter)
            .await
            .expect("failed to fetch newly created accounts for program");
        assert!(
            !accounts.is_empty(),
            "gTABD should return non empty list of accounts"
        );
        for account in accounts {
            let pubkey: Pubkey = account.pubkey.parse().unwrap();
            assert!(
                pubkey == acc1.pubkey || pubkey == acc2.pubkey,
                "getTokenAccountsByDelegate returned irrelevant account"
            );
            assert_eq!(
                account.account.data.decode().unwrap().len(),
                165,
                "token account data length mismatch in the result of getTokenAccountsByDelegate"
            );
        }
    }
    let nonexistent = env
        .rpc
        .get_token_accounts_by_owner(
            &owner,
            TokenAccountsFilter::Mint(Pubkey::new_unique()),
        )
        .await
        .expect("failed to fetch response for gTABO");
    assert!(
        nonexistent.is_empty(),
        "getTokenAccountsByOwner should not return anything for nonexistent mint"
    );
    let nonexistent = env
        .rpc
        .get_token_accounts_by_delegate(
            &owner,
            TokenAccountsFilter::ProgramId(Pubkey::new_unique()),
        )
        .await
        .expect("failed to fetch response for gTABD");
    assert!(
        nonexistent.is_empty(),
        "getTokenAccountsByDelegate should not return anything for nonexistent program"
    );
}
