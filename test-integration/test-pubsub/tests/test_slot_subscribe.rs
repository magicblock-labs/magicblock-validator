use futures::StreamExt;
use test_pubsub::PubSubEnv;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_slot_subscribe() {
    let env = PubSubEnv::new().await;
    let (mut rx, cancel) = env
        .ws_client
        .slot_subscribe()
        .await
        .expect("failed to subscribe to slot");
    let mut last_slot = 0;
    let mut i = 10;
    while let Some(s) = rx.next().await {
        assert!(
            s.slot > last_slot,
            "slot subscription should provide increasing slot sequence"
        );
        last_slot = s.slot;
        i -= 1;
        if i == 0 {
            break;
        }
    }
    cancel().await;
    assert_eq!(
        rx.next().await,
        None,
        "slot subscription should cancel properly"
    );
}
