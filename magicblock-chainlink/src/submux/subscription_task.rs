use std::{sync::Arc, time::Duration};

use futures_util::stream::{FuturesUnordered, StreamExt};
use solana_pubkey::Pubkey;
use tokio::sync::oneshot;
use tracing::*;

const SUBSCRIBE_TIMEOUT: Duration = Duration::from_millis(2_000);
const UNSUBSCRIBE_TIMEOUT: Duration = Duration::from_millis(1_000);

use crate::remote_account_provider::{
    chain_pubsub_client::{ChainPubsubClient, ReconnectableClient},
    errors::{RemoteAccountProviderError, RemoteAccountProviderResult},
};

#[derive(Clone)]
pub enum AccountSubscriptionTask {
    Subscribe(Pubkey, usize),
    SubscribeProgram(Pubkey, usize),
    Unsubscribe(Pubkey),
    Shutdown,
}

impl AccountSubscriptionTask {
    fn op_name(&self) -> &'static str {
        use AccountSubscriptionTask::*;
        match self {
            Subscribe(_, _) => "Subscribe",
            SubscribeProgram(_, _) => "SubscribeProgram",
            Unsubscribe(_) => "Unsubscribe",
            Shutdown => "Shutdown",
        }
    }
}

impl AccountSubscriptionTask {
    pub async fn process<T>(
        self,
        clients: Vec<Arc<T>>,
    ) -> RemoteAccountProviderResult<()>
    where
        T: ChainPubsubClient + ReconnectableClient + Send + Sync + 'static,
    {
        use AccountSubscriptionTask::*;

        let total_clients = clients.len();
        let required_confirmations = match &self {
            Subscribe(_, n) | SubscribeProgram(_, n) => *n,
            _ => 1,
        };

        // When no directly subscribing client is available then required_confirmations
        // will still resolve to 1, see: SubmuxClient::required_program_subscription_confirmations
        // This is intentional in order to avoid operating on stale data.
        // In this case we will always fail with not enough clients succeeded.
        // This will make any subscription fail and the validator will no longer be able to
        // operate properly, however this is safer than operating on stale data.
        // We rely on an alerting system to notify operators of this issue via the
        // connected_direct_pubsub_clients_gauge metric (when it goes to zero).
        // At this point the system needs to be troubleshot and restarted manually.
        // For accounts already in the bank we will have established a gRPC subscription for
        // redundancy and should be fine, but we should think about shutting down the validator
        // if all clients are down.

        // Validate inputs
        if total_clients == 0 {
            let op_name = self.op_name();
            return Err(
                RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                    format!("No clients provided for {op_name}"),
                ),
            );
        }

        if matches!(self, Subscribe(_, _) | SubscribeProgram(_, _))
            && required_confirmations == 0
        {
            return Err(
                RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                    "Required confirmations must be greater than zero"
                        .to_string(),
                ),
            );
        }

        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let mut futures = FuturesUnordered::new();
            for (i, client) in clients.iter().enumerate() {
                let client = client.clone();
                let task = self.clone();
                futures.push(async move {
                    let (result, count_as_success) = match task {
                        Subscribe(pubkey, _) => {
                            let result = match tokio::time::timeout(
                                SUBSCRIBE_TIMEOUT,
                                client.subscribe(pubkey),
                            )
                            .await
                            {
                                Ok(res) => res,
                                Err(_) => Err(RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                                    format!(
                                        "Subscribe timed out after {:?} for client {}",
                                        SUBSCRIBE_TIMEOUT,
                                        client.id()
                                    ),
                                )),
                            };
                            (result, client.subs_immediately())
                        }
                        SubscribeProgram(program_id, _) => {
                            let result = match tokio::time::timeout(
                                SUBSCRIBE_TIMEOUT,
                                client.subscribe_program(program_id),
                            )
                            .await
                            {
                                Ok(res) => res,
                                Err(_) => Err(RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                                    format!(
                                        "SubscribeProgram timed out after {:?} for client {}",
                                        SUBSCRIBE_TIMEOUT,
                                        client.id()
                                    ),
                                )),
                            };
                            (result, client.subs_immediately())
                        }
                        Unsubscribe(pubkey) => {
                            let result = match tokio::time::timeout(
                                UNSUBSCRIBE_TIMEOUT,
                                client.unsubscribe(pubkey),
                            )
                            .await
                            {
                                Ok(res) => res,
                                Err(_) => Err(RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                                    format!(
                                        "Unsubscribe timed out after {:?} for client {}",
                                        UNSUBSCRIBE_TIMEOUT,
                                        client.id()
                                    ),
                                )),
                            };
                            (result, true)
                        }
                        Shutdown => (client.shutdown().await, true),
                    };
                    (i, result, count_as_success)
                });
            }

            let mut errors = Vec::new();
            let mut tx = Some(tx);
            let mut successes = 0;
            let op_name = self.op_name();

            while let Some((i, result, count_as_success)) = futures.next().await
            {
                match result {
                    Ok(_) => {
                        if !count_as_success {
                            continue;
                        }
                        successes += 1;
                        if successes >= required_confirmations {
                            if let Some(tx) = tx.take() {
                                let _ = tx.send(Ok(()));
                            }
                        }
                    }
                    Err(e) => {
                        errors.push(format!("Client {}: {:?}", i, e));
                    }
                }
            }

            if let Some(tx) = tx {
                let msg = format!(
                    "Not enough clients succeeded to {}: {}. Required {}, got {}",
                    op_name.to_lowercase(),
                    errors.join(", "),
                    required_confirmations,
                    successes,
                );
                let _ = tx.send(Err(
                    RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                        msg,
                    ),
                ));
            } else if !errors.is_empty() {
                // If at least one client returned an `OK` response we only log a warning for the
                // ones that failed.
                // The failed clients will also trigger the reconnection logic
                // which takes care of fixing the RPC connection.
                warn!(
                    "Some clients failed to {}: {}",
                    op_name.to_lowercase(),
                    errors.join(", ")
                );
            }
        });

        rx.await.unwrap_or_else(|_| {
            Err(RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                "Orchestration task panicked or dropped channel".to_string(),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::*;
    use crate::remote_account_provider::chain_pubsub_client::mock::ChainPubsubClientMock;

    fn create_mock_client(
    ) -> (ChainPubsubClientMock, mpsc::Sender<()>, mpsc::Receiver<()>) {
        let (updates_sndr, updates_rcvr) = mpsc::channel(100);
        let (abort_sndr, abort_rcvr) = mpsc::channel(1);
        (
            ChainPubsubClientMock::new(updates_sndr, updates_rcvr),
            abort_sndr,
            abort_rcvr,
        )
    }

    #[tokio::test]
    async fn test_subscribe_single_confirmation() {
        let (mock_client, _abort_sndr, _abort_rcvr) = create_mock_client();
        let pubkey = Pubkey::new_unique();
        let task = AccountSubscriptionTask::Subscribe(pubkey, 1);

        let result = task.process(vec![Arc::new(mock_client)]).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_subscribe_multiple_confirmations() {
        let (mock_client1, _abort_sndr1, _abort_rcvr1) = create_mock_client();
        let (mock_client2, _abort_sndr2, _abort_rcvr2) = create_mock_client();
        let (mock_client3, _abort_sndr3, _abort_rcvr3) = create_mock_client();
        let pubkey = Pubkey::new_unique();
        let task = AccountSubscriptionTask::Subscribe(pubkey, 2);

        let result = task
            .process(vec![
                Arc::new(mock_client1),
                Arc::new(mock_client2),
                Arc::new(mock_client3),
            ])
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_subscribe_partial_client_failures_reaches_target() {
        let (mock_client1, _abort_sndr1, _abort_rcvr1) = create_mock_client();
        let (mock_client2, _abort_sndr2, _abort_rcvr2) = create_mock_client();
        let (mock_client3, _abort_sndr3, _abort_rcvr3) = create_mock_client();

        // Make client 2 fail
        mock_client2.simulate_disconnect();

        let pubkey = Pubkey::new_unique();
        let task = AccountSubscriptionTask::Subscribe(pubkey, 2);

        let result = task
            .process(vec![
                Arc::new(mock_client1),
                Arc::new(mock_client2),
                Arc::new(mock_client3),
            ])
            .await;

        // Should succeed after clients 1 and 3 confirm (2 confirmations)
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_subscribe_all_clients_fail() {
        let (mock_client1, _abort_sndr1, _abort_rcvr1) = create_mock_client();
        let (mock_client2, _abort_sndr2, _abort_rcvr2) = create_mock_client();
        let (mock_client3, _abort_sndr3, _abort_rcvr3) = create_mock_client();

        // Disconnect all clients
        mock_client1.simulate_disconnect();
        mock_client2.simulate_disconnect();
        mock_client3.simulate_disconnect();

        let pubkey = Pubkey::new_unique();
        let task = AccountSubscriptionTask::Subscribe(pubkey, 2);

        let result = task
            .process(vec![
                Arc::new(mock_client1),
                Arc::new(mock_client2),
                Arc::new(mock_client3),
            ])
            .await;

        assert!(result.is_err());
        eprintln!("Error: {}", result.as_ref().unwrap_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Not enough clients succeeded to subscribe"));
    }

    #[tokio::test]
    async fn test_subscribe_no_clients() {
        let pubkey = Pubkey::new_unique();
        let task = AccountSubscriptionTask::Subscribe(pubkey, 1);

        let result: RemoteAccountProviderResult<()> =
            task.process::<ChainPubsubClientMock>(vec![]).await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No clients provided"));
    }

    #[tokio::test]
    async fn test_subscribe_zero_confirmations() {
        let (mock_client1, _abort_sndr1, _abort_rcvr1) = create_mock_client();
        let (mock_client2, _abort_sndr2, _abort_rcvr2) = create_mock_client();

        let pubkey = Pubkey::new_unique();
        let task = AccountSubscriptionTask::Subscribe(pubkey, 0);

        let result = task
            .process(vec![Arc::new(mock_client1), Arc::new(mock_client2)])
            .await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Required confirmations must be greater than zero"));
    }

    #[tokio::test]
    async fn test_unsubscribe_ignores_confirmation_count() {
        let (mock_client1, _abort_sndr1, _abort_rcvr1) = create_mock_client();
        let (mock_client2, _abort_sndr2, _abort_rcvr2) = create_mock_client();

        let pubkey = Pubkey::new_unique();
        let task = AccountSubscriptionTask::Unsubscribe(pubkey);

        let result = task
            .process(vec![Arc::new(mock_client1), Arc::new(mock_client2)])
            .await;

        // Unsubscribe should succeed with single confirmation (default)
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_shutdown_ignores_confirmation_count() {
        let (mock_client1, _abort_sndr1, _abort_rcvr1) = create_mock_client();
        let (mock_client2, _abort_sndr2, _abort_rcvr2) = create_mock_client();

        let task = AccountSubscriptionTask::Shutdown;

        let result = task
            .process(vec![Arc::new(mock_client1), Arc::new(mock_client2)])
            .await;

        // Shutdown should succeed with single confirmation (default)
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_subscribe_insufficient_confirmations() {
        let (mock_client1, _abort_sndr1, _abort_rcvr1) = create_mock_client();
        let (mock_client2, _abort_sndr2, _abort_rcvr2) = create_mock_client();
        let (mock_client3, _abort_sndr3, _abort_rcvr3) = create_mock_client();

        // Make clients 2 and 3 fail
        mock_client2.simulate_disconnect();
        mock_client3.simulate_disconnect();

        let pubkey = Pubkey::new_unique();
        let task = AccountSubscriptionTask::Subscribe(pubkey, 2);

        let result = task
            .process(vec![
                Arc::new(mock_client1),
                Arc::new(mock_client2),
                Arc::new(mock_client3),
            ])
            .await;

        // Should fail because only 1 client succeeded but 2 required
        assert!(result.is_err());
    }
}
