use anyhow::Result;
use serde_json::json;
use solana_sdk::{signature::Keypair, signer::Signer};
use test_query_filtering::{
    assert_http_status, assert_missing_url_token_is_rejected, login,
    raw_rpc_with_bearer_header, raw_rpc_with_url_token,
};

#[test]
fn query_filtering_requires_a_valid_token() -> Result<()> {
    assert_missing_url_token_is_rejected("getSlot", &[])?;

    let err =
        raw_rpc_with_url_token(Some("not-a-valid-jwt"), "getSlot", json!([]))
            .unwrap_err();
    assert_http_status(&err, 401, "invalid token should return HTTP 401");
    Ok(())
}

#[test]
fn query_filtering_accepts_bearer_header_tokens() -> Result<()> {
    let user = Keypair::new();
    let token = login(&user)?;
    let response = raw_rpc_with_bearer_header(&token, "getSlot", json!([]))?;

    assert!(
        response.get("result").is_some(),
        "header token should authenticate {}",
        user.pubkey()
    );
    Ok(())
}
