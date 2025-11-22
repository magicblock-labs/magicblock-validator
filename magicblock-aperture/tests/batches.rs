use json::{JsonContainerTrait, JsonValueTrait, Value};
use setup::RpcTestEnv;

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
        "getIdentity should return adn object"
    );
}
