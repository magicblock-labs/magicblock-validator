use std::sync::Arc;

use jsonrpc_pubsub::Subscriber;
use magicblock_bank::bank::Bank;
use magicblock_geyser_plugin::rpc::GeyserRpcService;
use tokio::sync::mpsc;

use crate::{
    errors::{reject_internal_error, PubsubError, PubsubResult},
    handler::handle_subscription,
    subscription::SubscriptionRequest,
    types::{AccountParams, LogsParams, ProgramParams, SignatureParams},
    unsubscribe_tokens::UnsubscribeTokens,
};

// -----------------
// SubscriptionsReceiver
// -----------------
struct SubscriptionsReceiver {
    subscriptions: mpsc::Receiver<SubscriptionRequest>,
}

impl SubscriptionsReceiver {
    pub fn new(subscriptions: mpsc::Receiver<SubscriptionRequest>) -> Self {
        Self { subscriptions }
    }
}

// -----------------
// PubsubApi
// -----------------
#[derive(Clone)]
pub struct PubsubApi {
    subscribe: mpsc::Sender<SubscriptionRequest>,
    unsubscribe_tokens: UnsubscribeTokens,
}

impl PubsubApi {
    pub fn new() -> Self {
        let (subscribe_tx, subscribe_rx) = mpsc::channel(100);
        let unsubscribe_tokens = UnsubscribeTokens::new();
        {
            let unsubscribe_tokens = unsubscribe_tokens.clone();
            std::thread::spawn(move || {
                tokio::runtime::Builder::new_multi_thread()
                    // Note: there's no need to allocate more threads for the PubSub handling
                    .worker_threads(4)
                    .enable_all()
                    .thread_name("PubsubActorRuntime")
                    .build()
                    .unwrap()
                    .block_on(async move {
                        let mut subid: u64 = 0;
                        let mut actor =
                            SubscriptionsReceiver::new(subscribe_rx);

                        while let Some(subscription) =
                            actor.subscriptions.recv().await
                        {
                            subid += 1;
                            let unsubscriber = unsubscribe_tokens.add(subid);
                            tokio::spawn(handle_subscription(
                                subscription,
                                subid,
                                unsubscriber,
                            ));
                        }
                    });
            });
        }

        Self {
            subscribe: subscribe_tx,
            unsubscribe_tokens,
        }
    }

    pub fn account_subscribe(
        &self,
        subscriber: Subscriber,
        params: AccountParams,
        geyser_service: Arc<GeyserRpcService>,
    ) -> PubsubResult<()> {
        self.subscribe
            .blocking_send(SubscriptionRequest::Account {
                subscriber,
                params,
                geyser_service,
            })
            .map_err(map_send_error)?;

        Ok(())
    }

    pub fn program_subscribe(
        &self,
        subscriber: Subscriber,
        params: ProgramParams,
        geyser_service: Arc<GeyserRpcService>,
    ) -> PubsubResult<()> {
        self.subscribe
            .blocking_send(SubscriptionRequest::Program {
                subscriber,
                params,
                geyser_service,
            })
            .map_err(map_send_error)?;

        Ok(())
    }

    pub fn slot_subscribe(
        &self,
        subscriber: Subscriber,
        geyser_service: Arc<GeyserRpcService>,
    ) -> PubsubResult<()> {
        self.subscribe
            .blocking_send(SubscriptionRequest::Slot {
                subscriber,
                geyser_service,
            })
            .map_err(map_send_error)?;

        Ok(())
    }

    pub fn signature_subscribe(
        &self,
        subscriber: Subscriber,
        params: SignatureParams,
        geyser_service: Arc<GeyserRpcService>,
        bank: Arc<Bank>,
    ) -> PubsubResult<()> {
        self.subscribe
            .blocking_send(SubscriptionRequest::Signature {
                subscriber,
                params,
                geyser_service,
                bank,
            })
            .map_err(map_send_error)?;

        Ok(())
    }

    pub fn logs_subscribe(
        &self,
        subscriber: Subscriber,
        params: LogsParams,
        geyser_service: Arc<GeyserRpcService>,
    ) -> PubsubResult<()> {
        self.subscribe
            .blocking_send(SubscriptionRequest::Logs {
                subscriber,
                params,
                geyser_service,
            })
            .map_err(map_send_error)?;

        Ok(())
    }

    pub fn unsubscribe(&self, id: u64) {
        self.unsubscribe_tokens.unsubscribe(id);
    }
}

fn map_send_error(
    err: mpsc::error::SendError<SubscriptionRequest>,
) -> PubsubError {
    let err_msg = format!("{:?}", err);
    let subscription = err.0;
    let subscriber = subscription.into_subscriber();
    reject_internal_error(
        subscriber,
        "Failed to subscribe",
        Some(err_msg.clone()),
    );

    PubsubError::FailedToSendSubscription(err_msg)
}
