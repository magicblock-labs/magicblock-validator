use json::{JsonContainerTrait, JsonValueTrait, Value};
use setup::RpcTestEnv;

const REMOTE_ACCOUNT_CLAIMS_HEADER: &str = "X-MB-Remote-Account-Claims";

fn remote_account_claims_header(response: &reqwest::Response) -> u64 {
    response
        .headers()
        .get(REMOTE_ACCOUNT_CLAIMS_HEADER)
        .expect("remote account claims header should be present")
        .to_str()
        .expect("remote account claims header should be valid ASCII")
        .parse::<u64>()
        .expect("remote account claims header should be an integer")
}

mod setup;

#[tokio::test]
async fn test_batch_requests() {
    let env = RpcTestEnv::new().await;
    let client = reqwest::Client::new();
    let rpc_url = env.rpc.url();

    // Construct a batch request using serde_json macro
    let batch_request = json::json!([
        {"jsonrpc": "2.0", "method": "getVersion", "id": 1},
        {"jsonrpc": "2.0", "method": "getIdentity", "id": 2}
    ]);

    let response = client
        .post(rpc_url)
        .json(&batch_request)
        .send()
        .await
        .expect("Failed to send batch request");

    assert!(
        response.status().is_success(),
        "HTTP request failed status: {}",
        response.status()
    );
    let text = response.text().await.unwrap();
    let body: Value = json::from_str(&text).unwrap();

    assert!(body.is_array(), "Response should be a JSON array");
    let results = body.as_array().unwrap();
    assert_eq!(results.len(), 2, "Should return exactly 2 results");

    // Helper to find result by ID since batch responses can be out of order
    let get_result = |id: u64| {
        results
            .iter()
            .find(|v| v["id"] == id)
            .expect("Result for id not found")
    };

    // Verify getVersion result (ID 1)
    let res1 = get_result(1);
    assert!(
        res1.get("result").is_some(),
        "Should contain a result object"
    );
    assert!(
        res1["result"]["solana-core"].is_str(),
        "Should contain solana-core version"
    );

    // Verify getIdentity result (ID 2)
    let res2 = get_result(2);
    assert!(
        res2["result"].is_object(),
        "getIdentity should return an object"
    );
}

#[tokio::test]
async fn test_batch_requests_emit_remote_account_claims_header_zero_when_no_fetches_triggered(
) {
    let env = RpcTestEnv::new().await;
    let client = reqwest::Client::new();
    let rpc_url = env.rpc.url();
    let batch_request = json::json!([
        {"jsonrpc": "2.0", "method": "getVersion", "id": 1},
        {"jsonrpc": "2.0", "method": "getIdentity", "id": 2}
    ]);

    let response = client
        .post(rpc_url)
        .json(&batch_request)
        .send()
        .await
        .expect("batch HTTP request should succeed");

    assert!(response.status().is_success());
    assert_eq!(remote_account_claims_header(&response), 0);
    let body: json::Value = response
        .json()
        .await
        .expect("batch response body should decode");
    assert!(body.is_array(), "batch response should be an array");
}
