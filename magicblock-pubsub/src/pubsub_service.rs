use std::{
    net::{IpAddr, SocketAddr},
    sync::{Arc, RwLock},
    thread,
};

use jsonrpc_core::{futures, BoxFuture, MetaIoHandler, Params};
use jsonrpc_pubsub::{
    PubSubHandler, Session, Subscriber, SubscriptionId, UnsubscribeRpcMethod,
};
use jsonrpc_ws_server::{CloseHandle, RequestContext, Server, ServerBuilder};
use log::*;
use magicblock_bank::bank::Bank;
use magicblock_geyser_plugin::rpc::GeyserRpcService;
use serde_json::Value;
use solana_sdk::rpc_port::DEFAULT_RPC_PUBSUB_PORT;

use crate::{
    errors::{ensure_and_try_parse_params, ensure_empty_params, PubsubResult},
    pubsub_api::PubsubApi,
    types::{AccountParams, LogsParams, ProgramParams, SignatureParams},
};

// -----------------
// PubsubConfig
// -----------------
#[derive(Clone)]
pub struct PubsubConfig {
    socket: SocketAddr,
    max_connections: usize,
}

impl PubsubConfig {
    pub fn from_rpc(rpc_addr: IpAddr, rpc_port: u16) -> Self {
        Self {
            socket: SocketAddr::new(rpc_addr, rpc_port + 1),
            max_connections: 16384,
        }
    }
}

impl Default for PubsubConfig {
    fn default() -> Self {
        Self {
            socket: SocketAddr::from(([0, 0, 0, 0], DEFAULT_RPC_PUBSUB_PORT)),
            max_connections: 16384,
        }
    }
}

impl PubsubConfig {
    pub fn socket(&self) -> &SocketAddr {
        &self.socket
    }
}

pub type PubsubServiceCloseHandle = Arc<RwLock<Option<CloseHandle>>>;
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

        service
            .add_account_subscribe()
            .add_program_subscribe()
            .add_slot_subscribe()
            .add_signature_subscribe()
            .add_logs_subscribe()
    }

    #[allow(clippy::result_large_err)]
    pub fn start(self) -> jsonrpc_ws_server::Result<Server> {
        let extractor =
            |context: &RequestContext| Arc::new(Session::new(context.sender()));

        ServerBuilder::with_meta_extractor(self.io, extractor)
            // NOTE: we just set the max number of allowed connections to a reasonably high value
            // to satisfy most of the use cases, however this number cannot be arbitrarily large
            // due to the preallocation involved, and a large value will trigger an OOM Kill
            .max_connections(self.config.max_connections)
            .start(&self.config.socket)
    }

    pub fn spawn_new(
        config: PubsubConfig,
        geyser_rpc_service: Arc<GeyserRpcService>,
        bank: Arc<Bank>,
    ) -> PubsubResult<(thread::JoinHandle<()>, PubsubServiceCloseHandle)> {
        let socket = *config.socket();
        let service = PubsubService::new(config, geyser_rpc_service, bank);
        Self::spawn(service, &socket)
    }

    /// Spawns the [PubsubService] on a separate thread and waits for it to
    /// complete. Thus joining the returned [std::thread::JoinHandle] will block
    /// until the service is stopped.
    pub fn spawn(
        self,
        socket: &SocketAddr,
    ) -> PubsubResult<(thread::JoinHandle<()>, PubsubServiceCloseHandle)> {
        let socket = format!("{:?}", socket);
        let close_handle: PubsubServiceCloseHandle = Default::default();
        let thread_handle = {
            let close_handle_rc = close_handle.clone();
            thread::spawn(move || {
                let server = match self.start() {
                    Ok(server) => server,
                    Err(err) => {
                        error!("Failed to start pubsub server: {:?}", err);
                        return;
                    }
                };

                info!("Pubsub server started on {}", socket);
                let close_handle = server.close_handle().clone();
                close_handle_rc.write().unwrap().replace(close_handle);
                let _ = server.wait();
            })
        };
        Ok((thread_handle, close_handle))
    }

    pub fn close(close_handle: &PubsubServiceCloseHandle) {
        if let Some(close_handle) = close_handle.write().unwrap().take() {
            close_handle.close();
        }
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

    fn add_program_subscribe(mut self) -> Self {
        let subscribe = {
            let api = self.api.clone();
            let geyser_service = self.geyser_service.clone();
            move |params: Params, _, subscriber: Subscriber| {
                let (subscriber, program_params): (Subscriber, ProgramParams) =
                    match ensure_and_try_parse_params(subscriber, params) {
                        Some((subscriber, params)) => (subscriber, params),
                        None => {
                            return;
                        }
                    };

                debug!("{:#?}", program_params);

                if let Err(err) = api.program_subscribe(
                    subscriber,
                    program_params,
                    geyser_service.clone(),
                ) {
                    error!("Failed to handle program subscribe: {:?}", err);
                };
            }
        };
        let unsubscribe = self.create_unsubscribe();

        let io = &mut self.io;
        io.add_subscription(
            "programNotification",
            ("programSubscribe", subscribe),
            ("programUnsubscribe", unsubscribe),
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
                    error!("Failed to handle slot subscribe: {:?}", err);
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

    fn add_signature_subscribe(mut self) -> Self {
        let subscribe = {
            let api = self.api.clone();
            let geyser_service = self.geyser_service.clone();
            let bank = self.bank.clone();
            move |params: Params, _, subscriber: Subscriber| {
                let (subscriber, params): (Subscriber, SignatureParams) =
                    match ensure_and_try_parse_params(subscriber, params) {
                        Some((subscriber, params)) => (subscriber, params),
                        None => {
                            return;
                        }
                    };

                if let Err(err) = api.signature_subscribe(
                    subscriber,
                    params,
                    geyser_service.clone(),
                    bank.clone(),
                ) {
                    error!("Failed to handle signature subscribe: {:?}", err);
                };
            }
        };
        let unsubscribe = self.create_unsubscribe();

        let io = &mut self.io;
        io.add_subscription(
            "signatureNotification",
            ("signatureSubscribe", subscribe),
            ("signatureUnsubscribe", unsubscribe),
        );

        self
    }

    fn add_logs_subscribe(mut self) -> Self {
        let subscribe = {
            let api = self.api.clone();
            let geyser_service = self.geyser_service.clone();
            move |params: Params, _, subscriber: Subscriber| {
                let (subscriber, logs_params): (Subscriber, LogsParams) =
                    match ensure_and_try_parse_params(subscriber, params) {
                        Some((subscriber, params)) => (subscriber, params),
                        None => {
                            return;
                        }
                    };

                debug!("{:#?}", logs_params);

                if let Err(err) = api.logs_subscribe(
                    subscriber,
                    logs_params,
                    geyser_service.clone(),
                ) {
                    error!("Failed to handle logs subscribe: {:?}", err);
                };
            }
        };
        let unsubscribe = self.create_unsubscribe();

        let io = &mut self.io;
        io.add_subscription(
            "logsNotification",
            ("logsSubscribe", subscribe),
            ("logsUnsubscribe", unsubscribe),
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
