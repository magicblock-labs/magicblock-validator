use jsonrpc_core::{futures, BoxFuture, MetaIoHandler, Params};
use log::*;
use serde_json::Value;
use sleipnir_bank::bank::Bank;
use sleipnir_geyser_plugin::rpc::GeyserRpcService;
use solana_sdk::rpc_port::DEFAULT_RPC_PUBSUB_PORT;
use std::{net::SocketAddr, sync::Arc, thread};

use crate::{
    errors::{ensure_and_try_parse_params, ensure_empty_params},
    pubsub_api::PubsubApi,
    types::AccountParams,
};
use jsonrpc_pubsub::{
    PubSubHandler, Session, Subscriber, SubscriptionId, UnsubscribeRpcMethod,
};
use jsonrpc_ws_server::{RequestContext, Server, ServerBuilder};

// -----------------
// PubsubConfig
// -----------------
pub struct PubsubConfig {
    socket: SocketAddr,
}

impl Default for PubsubConfig {
    fn default() -> Self {
        let socket =
            SocketAddr::from(([127, 0, 0, 1], DEFAULT_RPC_PUBSUB_PORT));
        Self { socket }
    }
}

impl PubsubConfig {
    pub fn socket(&self) -> &SocketAddr {
        &self.socket
    }
}

pub struct PubsubService {
    api: PubsubApi,
    geyser_service: Arc<GeyserRpcService>,
    config: PubsubConfig,
    io: PubSubHandler<Arc<Session>>,
    bank: Arc<Bank>,
}

impl PubsubService {
    pub fn new(
        config: PubsubConfig,
        geyser_rpc_service: Arc<GeyserRpcService>,
        bank: Arc<Bank>,
    ) -> Self {
        let io = PubSubHandler::new(MetaIoHandler::default());
        let service = Self {
            api: PubsubApi::new(),
            config,
            io,
            geyser_service: geyser_rpc_service,
            bank,
        };

        service.add_account_subscribe().add_slot_subscribe()
    }

    #[allow(clippy::result_large_err)]
    pub fn start(self) -> jsonrpc_ws_server::Result<Server> {
        ServerBuilder::with_meta_extractor(
            self.io,
            |context: &RequestContext| Arc::new(Session::new(context.sender())),
        )
        .start(&self.config.socket)
    }

    pub fn spawn(
        config: PubsubConfig,
        geyser_rpc_service: Arc<GeyserRpcService>,
        bank: Arc<Bank>,
    ) -> thread::JoinHandle<()> {
        let socket = format!("{:?}", config.socket());
        thread::spawn(move || {
            let service = PubsubService::new(config, geyser_rpc_service, bank);
            let server = match service.start() {
                Ok(server) => server,
                Err(err) => {
                    error!("Failed to start pubsub server: {:?}", err);
                    return;
                }
            };

            info!("Pubsub server started on {}", socket);
            let _ = server.wait();
        })
    }

    fn add_account_subscribe(mut self) -> Self {
        let subscribe = {
            let api = self.api.clone();
            let geyser_service = self.geyser_service.clone();
            move |params: Params, _, subscriber: Subscriber| {
                let (subscriber, account_params): (Subscriber, AccountParams) =
                    match ensure_and_try_parse_params(subscriber, params) {
                        Some((subscriber, params)) => (subscriber, params),
                        None => {
                            return;
                        }
                    };

                debug!("{:#?}", account_params);

                if let Err(err) = api.account_subscribe(
                    subscriber,
                    account_params,
                    geyser_service.clone(),
                ) {
                    error!("Failed to handle account subscribe: {:?}", err);
                };
            }
        };
        let unsubscribe = self.create_unsubscribe();

        let io = &mut self.io;
        io.add_subscription(
            "accountNotification",
            ("accountSubscribe", subscribe),
            ("accountUnsubscribe", unsubscribe),
        );

        self
    }

    fn add_slot_subscribe(mut self) -> Self {
        let subscribe = {
            let api = self.api.clone();
            let geyser_service = self.geyser_service.clone();
            move |params: Params, _, subscriber: Subscriber| {
                let subscriber =
                    match ensure_empty_params(subscriber, &params, true) {
                        Some(subscriber) => subscriber,
                        None => return,
                    };

                if let Err(err) =
                    api.slot_subscribe(subscriber, geyser_service.clone())
                {
                    error!("Failed to handle solt subscribe: {:?}", err);
                };
            }
        };
        let unsubscribe = self.create_unsubscribe();

        let io = &mut self.io;
        io.add_subscription(
            "slotNotification",
            ("slotSubscribe", subscribe),
            ("slotUnsubscribe", unsubscribe),
        );

        self
    }

    fn create_unsubscribe(&self) -> impl UnsubscribeRpcMethod<Arc<Session>> {
        let actor = self.api.clone();
        move |id: SubscriptionId,
              _session: Option<Arc<Session>>|
              -> BoxFuture<jsonrpc_core::Result<Value>> {
            match id {
                SubscriptionId::Number(id) => {
                    actor.unsubscribe(id);
                }
                SubscriptionId::String(_) => {
                    warn!("subscription id should be a number")
                }
            }
            Box::pin(futures::future::ready(Ok(Value::Bool(true))))
        }
    }
}
