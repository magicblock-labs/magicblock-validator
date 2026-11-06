#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::HashSet;

use base64::{Engine as _, prelude::BASE64_STANDARD};
use keeper::testkit::{load_v42_lamports, store_v42};
use setup::{PROGRAM_ID, RpcTestEnv, TOKEN_PROGRAM_ID, remote_account_claims_header};
use solana_account::{AccountMode, ReadableAccount, accounts_equal};
use solana_account_decoder::MAX_BASE58_BYTES;
use solana_pubkey::Pubkey;
use solana_rpc_client_api::{
    config::RpcProgramAccountsConfig,
    filter::{Memcmp, MemcmpEncodedBytes, RpcFilterType},
    request::{MAX_MULTIPLE_ACCOUNTS, TokenAccountsFilter},
};

mod setup;

/// Verifies `getAccountInfo` for both existing and non-existent accounts.
#[tokio::test]
async fn test_get_account_info() {
    let env = RpcTestEnv::new().await;

    // Test for an existing account
    let key = store_v42(&env.engine, 0, AccountMode::Magic);
    let expected = env.engine.get_account(key).expect("stored account");
    let account = env.rpc.get_account(&key).await.expect("failed to fetch created account");
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

#[tokio::test]
async fn test_get_account_info_emits_remote_account_claims_header_zero_for_bank_hit() {
    let env = RpcTestEnv::new().await;
    let acc = store_v42(&env.engine, 0, AccountMode::Magic);
    let client = reqwest::Client::new();
    let request = json::json!({
        "jsonrpc": "2.0",
        "method": "getAccountInfo",
        "params": [acc.to_string()],
        "id": 1,
    });

    let response = client
        .post(env.rpc.url())
        .json(&request)
        .send()
        .await
        .expect("getAccountInfo HTTP request should succeed");

    assert!(response.status().is_success());
    assert_eq!(remote_account_claims_header(&response), 0);
    let body: json::Value =
        json::from_str(&response.text().await.expect("getAccountInfo response body should decode"))
            .expect("getAccountInfo response body should be valid JSON");
    assert!(body["result"].is_object(), "response should contain result");
}

/// Verifies account ordering and null results, and enforces the query-size limit
/// on the raw HTTP path rather than the SDK's client-side request handling.
#[tokio::test]
async fn test_get_multiple_accounts() {
    let env = RpcTestEnv::new().await;

    // Test with a list of existing accounts
    let acc1 = store_v42(&env.engine, 1, AccountMode::Magic);
    let acc2 = store_v42(&env.engine, 2, AccountMode::Magic);
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
        .expect("rpc request for mixed existing and non-existent accounts failed");
    assert_eq!(
        mixed.len(),
        3,
        "should return one entry per requested pubkey"
    );
    assert!(
        accounts_equal(
            mixed[0].as_ref().expect("existing first account should be returned"),
            env.engine.get_account(acc1).as_ref().expect("stored account")
        ),
        "first result should match the first requested account"
    );
    assert_eq!(
        mixed[1], None,
        "missing middle account should render as null"
    );
    assert!(
        accounts_equal(
            mixed[2].as_ref().expect("existing last account should be returned"),
            env.engine.get_account(acc2).as_ref().expect("stored account")
        ),
        "last result should match the last requested account"
    );

    let client = reqwest::Client::new();
    for count in [MAX_MULTIPLE_ACCOUNTS, MAX_MULTIPLE_ACCOUNTS + 1] {
        let response = client
            .post(env.rpc.url())
            .json(&json::json!({
                "jsonrpc": "2.0",
                "method": "getMultipleAccounts",
                "params": [vec![acc1.to_string(); count]],
                "id": 1,
            }))
            .send()
            .await
            .expect("account query should receive a response");
        assert_eq!(remote_account_claims_header(&response), 0);
        let body: json::Value = response.json().await.expect("response should decode");
        if count == MAX_MULTIPLE_ACCOUNTS {
            assert_eq!(body["result"]["value"].as_array().unwrap().len(), count);
        } else {
            assert_eq!(body["error"]["code"], -32602);
            assert!(body.get("result").is_none());
        }
    }
}

#[tokio::test]
async fn test_get_multiple_accounts_emits_remote_account_claims_header_zero_for_bank_hits() {
    let env = RpcTestEnv::new().await;
    let acc1 = store_v42(&env.engine, 0, AccountMode::Magic);
    let acc2 = store_v42(&env.engine, 0, AccountMode::Magic);
    let client = reqwest::Client::new();
    let request = json::json!({
        "jsonrpc": "2.0",
        "method": "getMultipleAccounts",
        "params": [[acc1.to_string(), acc2.to_string()]],
        "id": 1,
    });

    let response = client
        .post(env.rpc.url())
        .json(&request)
        .send()
        .await
        .expect("getMultipleAccounts HTTP request should succeed");

    assert!(response.status().is_success());
    assert_eq!(remote_account_claims_header(&response), 0);
    let body: json::Value = json::from_str(
        &response.text().await.expect("getMultipleAccounts response body should decode"),
    )
    .expect("getMultipleAccounts response body should be valid JSON");
    assert!(body["result"].is_object(), "response should contain result");
}

/// Verifies `getBalance` for both existing and non-existent accounts.
#[tokio::test]
async fn test_get_balance() {
    let env = RpcTestEnv::new().await;

    // Test balance of an existing account
    let acc = store_v42(&env.engine, 0, AccountMode::Magic);
    let balance = env
        .rpc
        .get_balance(&acc)
        .await
        .expect("failed to fetch balance for newly created account");
    assert_eq!(
        balance,
        load_v42_lamports(&env.engine, acc).expect("stored balance"),
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

#[tokio::test]
async fn test_get_balance_emits_remote_account_claims_header_zero_for_bank_hit() {
    let env = RpcTestEnv::new().await;
    let acc = store_v42(&env.engine, 0, AccountMode::Magic);
    let client = reqwest::Client::new();
    let request = json::json!({
        "jsonrpc": "2.0",
        "method": "getBalance",
        "params": [acc.to_string()],
        "id": 1,
    });

    let response = client
        .post(env.rpc.url())
        .json(&request)
        .send()
        .await
        .expect("getBalance HTTP request should succeed");

    assert!(response.status().is_success());
    assert_eq!(remote_account_claims_header(&response), 0);
    let body: json::Value =
        json::from_str(&response.text().await.expect("getBalance response body should decode"))
            .expect("getBalance response body should be valid JSON");
    assert!(body["result"].is_object(), "response should contain result");
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
    let nonexistent_result = env.rpc.get_token_account_balance(&Pubkey::new_unique()).await;
    assert!(
        nonexistent_result.is_err(),
        "fetching balance of a non-token account should result in an error"
    );
}

#[tokio::test]
async fn test_get_token_account_balance_emits_remote_account_claims_header_zero_for_bank_hit() {
    let env = RpcTestEnv::new().await;
    let token_account = env.create_token_account(Pubkey::new_unique(), Pubkey::new_unique());
    let client = reqwest::Client::new();
    let request = json::json!({
        "jsonrpc": "2.0",
        "method": "getTokenAccountBalance",
        "params": [token_account.to_string()],
        "id": 1,
    });

    let response = client
        .post(env.rpc.url())
        .json(&request)
        .send()
        .await
        .expect("getTokenAccountBalance HTTP request should succeed");

    assert!(response.status().is_success());
    assert_eq!(remote_account_claims_header(&response), 0);
    let body: json::Value = json::from_str(
        &response
            .text()
            .await
            .expect("getTokenAccountBalance response body should decode"),
    )
    .expect("getTokenAccountBalance response body should be valid JSON");
    assert!(body["result"].is_object(), "response should contain result");
}

#[tokio::test]
async fn test_get_delegation_status_emits_remote_account_claims_header_zero_for_bank_hit() {
    let env = RpcTestEnv::new().await;
    let acc = store_v42(&env.engine, 0, AccountMode::Magic);
    let client = reqwest::Client::new();
    let request = json::json!({
        "jsonrpc": "2.0",
        "method": "getDelegationStatus",
        "params": [acc.to_string()],
        "id": 1,
    });

    let response = client
        .post(env.rpc.url())
        .json(&request)
        .send()
        .await
        .expect("getDelegationStatus HTTP request should succeed");

    assert!(response.status().is_success());
    assert_eq!(remote_account_claims_header(&response), 0);
    let body: json::Value = json::from_str(
        &response.text().await.expect("getDelegationStatus response body should decode"),
    )
    .expect("getDelegationStatus response body should be valid JSON");
    assert!(body["result"].is_object(), "response should contain result");
}

/// Verifies `getProgramAccounts` finds all accounts owned by a program.
#[tokio::test]
async fn test_get_program_accounts() {
    let env = RpcTestEnv::new().await;

    // Test a program with multiple accounts
    let acc1 = store_v42(&env.engine, 1, AccountMode::Magic);
    let acc2 = store_v42(&env.engine, 2, AccountMode::Magic);
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

    // All supported memcmp wire encodings select the same account.
    for bytes in [
        MemcmpEncodedBytes::Base58(bs58::encode([1]).into_string()),
        MemcmpEncodedBytes::Base64("AQ==".into()),
        MemcmpEncodedBytes::Bytes(vec![1]),
    ] {
        let accounts = env
            .rpc
            .get_program_ui_accounts_with_config(
                &PROGRAM_ID,
                RpcProgramAccountsConfig {
                    filters: Some(vec![RpcFilterType::Memcmp(Memcmp::new(0, bytes))]),
                    ..Default::default()
                },
            )
            .await
            .expect("filtered program accounts");
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].0, acc1);
    }

    // Test a program with no accounts
    let empty_program_accounts = env.rpc.get_program_accounts(&Pubkey::new_unique()).await.unwrap();
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
        .get_token_accounts_by_owner(&owner, TokenAccountsFilter::Mint(Pubkey::new_unique()))
        .await;
    assert!(nonexistent.is_err(), "a missing mint should be rejected");
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
        .await;
    assert!(
        nonexistent.is_err(),
        "an unknown token program should be rejected"
    );
}

async fn post_rpc(env: &RpcTestEnv, method: &str, params: json::Value) -> json::Value {
    reqwest::Client::new()
        .post(env.rpc.url())
        .json(&json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
        .send()
        .await
        .expect("rpc request should receive a response")
        .json()
        .await
        .expect("response should decode")
}

/// Base58 account data over 128 bytes is rejected as Agave does, instead of
/// returning a placeholder string as the account data.
#[tokio::test]
async fn test_base58_account_data_size_limit() {
    let env = RpcTestEnv::new().await;
    let mint = Pubkey::new_unique();
    let owner = Pubkey::new_unique();
    // SPL token accounts are 165 bytes, over the 128-byte base58 limit.
    let token = env.create_token_account(mint, owner);
    let data = env.engine.get_account(token).expect("stored token account").data().to_vec();
    assert!(data.len() > MAX_BASE58_BYTES);

    let token = token.to_string();
    let base58 = json::json!({ "encoding": "base58" });
    for (method, params) in [
        ("getAccountInfo", json::json!([token])),
        ("getAccountInfo", json::json!([token, base58])),
        ("getMultipleAccounts", json::json!([[token], base58])),
        (
            "getProgramAccounts",
            json::json!([TOKEN_PROGRAM_ID.to_string(), base58]),
        ),
        (
            "getTokenAccountsByOwner",
            json::json!([owner.to_string(), { "mint": mint.to_string() }, base58]),
        ),
    ] {
        let body = post_rpc(&env, method, params).await;
        assert_eq!(
            body["error"]["code"], -32600,
            "{method} should reject oversized base58 data: {body}"
        );
        assert!(
            body.get("result").is_none(),
            "{method} should not return a result"
        );
    }

    // A slice within the limit is still encoded.
    let sliced = json::json!({
        "encoding": "base58",
        "dataSlice": { "offset": 0, "length": MAX_BASE58_BYTES },
    });
    let body = post_rpc(&env, "getAccountInfo", json::json!([token, sliced])).await;
    assert_eq!(
        body["result"]["value"]["data"],
        json::json!([bs58::encode(&data[..MAX_BASE58_BYTES]).into_string(), "base58"]),
    );

    // Like Agave, getMultipleAccounts defaults to base64.
    let body = post_rpc(&env, "getMultipleAccounts", json::json!([[token]])).await;
    assert_eq!(
        body["result"]["value"][0]["data"],
        json::json!([BASE64_STANDARD.encode(&data), "base64"]),
    );
}
