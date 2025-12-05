use std::sync::Arc;

use futures_util::stream::{FuturesUnordered, StreamExt};
use log::*;
use solana_pubkey::Pubkey;
use tokio::sync::oneshot;

use crate::remote_account_provider::{
    chain_pubsub_client::{ChainPubsubClient, ReconnectableClient},
    errors::{RemoteAccountProviderError, RemoteAccountProviderResult},
};

#[derive(Clone)]
pub enum AccountSubscriptionTask {
    Subscribe(Pubkey, usize),
    Unsubscribe(Pubkey),
    Shutdown,
}

impl AccountSubscriptionTask {
    fn op_name(&self) -> &'static str {
        use AccountSubscriptionTask::*;
        match self {
            Subscribe(_, _) => "Subscribe",
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
            Subscribe(_, n) => *n,
            _ => 1,
        };

        // Validate inputs
        if total_clients == 0 {
            let op_name = self.op_name();
            return Err(
                RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                    format!("No clients provided for {op_name}"),
                ),
            );
        }

        if let Subscribe(_, _) = self {
            if required_confirmations == 0 {
                return Err(
                    RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                        "Required confirmations must be greater than zero"
                            .to_string(),
                    ),
                );
            }
        }

        let (tx, rx) = oneshot::channel();
        let target_successes =
            std::cmp::min(required_confirmations, total_clients);

        tokio::spawn(async move {
            let mut futures = FuturesUnordered::new();
            for (i, client) in clients.iter().enumerate() {
                let client = client.clone();
                let task = self.clone();
                futures.push(async move {
                    let result = match task {
                        Subscribe(pubkey, _) => client.subscribe(pubkey).await,
                        Unsubscribe(pubkey) => client.unsubscribe(pubkey).await,
                        Shutdown => client.shutdown().await,
                    };
                    (i, result)
                });
            }

            let mut errors = Vec::new();
            let mut tx = Some(tx);
            let mut successes = 0;
            let op_name = self.op_name();

            while let Some((i, result)) = futures.next().await {
                match result {
                    Ok(_) => {
                        successes += 1;
                        if successes >= target_successes {
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
                    "All clients failed to {}: {}",
                    op_name.to_lowercase(),
                    errors.join(", ")
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
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("All clients failed"));
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
