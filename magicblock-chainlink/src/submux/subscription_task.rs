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
    Subscribe(Pubkey),
    Unsubscribe(Pubkey),
    Shutdown,
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
        let (tx, rx) = oneshot::channel();

        tokio::spawn(async move {
            let mut futures = FuturesUnordered::new();
            for (i, client) in clients.iter().enumerate() {
                let client = client.clone();
                let task = self.clone();
                futures.push(async move {
                    let result = match task {
                        Subscribe(pubkey) => client.subscribe(pubkey).await,
                        Unsubscribe(pubkey) => client.unsubscribe(pubkey).await,
                        Shutdown => {
                            client.shutdown().await;
                            Ok(())
                        }
                    };
                    (i, result)
                });
            }

            let mut errors = Vec::new();
            let mut tx = Some(tx);
            let op_name = match self {
                Subscribe(_) => "Subscribe",
                Unsubscribe(_) => "Unsubscribe",
                Shutdown => "Shutdown",
            };

            while let Some((i, result)) = futures.next().await {
                match result {
                    Ok(_) => {
                        if let Some(tx) = tx.take() {
                            let _ = tx.send(Ok(()));
                        }
                    }
                    Err(e) => {
                        if tx.is_none() {
                            // If at least one client returned an `OK` response, ignore any `ERR` responses
                            // after that. These clients will also trigger the reconnection logic
                            // which takes care of fixing the RPC connection.
                            warn!(
                                "{} failed for client {}: {:?}",
                                op_name, i, e
                            );
                        } else {
                            errors.push(format!("Client {}: {:?}", i, e));
                        }
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
            }
        });

        rx.await.unwrap_or_else(|_| {
            Err(RemoteAccountProviderError::AccountSubscriptionsTaskFailed(
                "Orchestration task panicked or dropped channel".to_string(),
            ))
        })
    }
}
