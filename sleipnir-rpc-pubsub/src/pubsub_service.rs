use jsonrpc_ws_server::{RequestContext, Server, ServerBuilder};
use log::*;
use serde_json::Value;
use std::{net::SocketAddr, sync::Arc};

use jsonrpc_core::{futures, BoxFuture, MetaIoHandler, Params};
use jsonrpc_pubsub::{PubSubHandler, Session, Subscriber, SubscriptionId};
use sleipnir_rpc_client_api::{
    // config::{RpcAccountInfoConfig, RpcSignatureSubscribeConfig},
    response::{
        ProcessedSignatureResult, Response, RpcResponseContext,
        RpcSignatureResult,
    },
};
use solana_sdk::rpc_port::DEFAULT_RPC_PUBSUB_PORT;

use crate::pubsub_types::{ResponseWithSubscriptionId, SignatureParams};

pub struct RpcPubsubConfig {
    socket: SocketAddr,
}

impl Default for RpcPubsubConfig {
    fn default() -> Self {
        let socket =
            SocketAddr::from(([127, 0, 0, 1], DEFAULT_RPC_PUBSUB_PORT));
        Self { socket }
    }
}

impl RpcPubsubConfig {
    pub fn socket(&self) -> &SocketAddr {
        &self.socket
    }
}

pub struct RpcPubsubService {
    config: RpcPubsubConfig,
    io: PubSubHandler<Arc<Session>>,
}

impl RpcPubsubService {
    pub fn new(config: RpcPubsubConfig) -> Self {
        let io = PubSubHandler::new(MetaIoHandler::default());
        Self { config, io }
    }

    pub fn add_signature_subscribe(mut self) -> Self {
        self.io.add_subscription(
            "signatureNotification",
            (
                "signatureSubscribe",
                |params: Params, _, subscriber: Subscriber| {
                    if params == Params::None {
                        subscriber
                            .reject(jsonrpc_core::Error {
                                code: jsonrpc_core::ErrorCode::ParseError,
                                message:
                                    "Missing parameters. Subscription rejected."
                                        .to_string(),
                                data: None,
                            })
                            .unwrap();
                        return;
                    }

                    let signature_params: SignatureParams = match params
                        .parse()
                        .map_err(|err| jsonrpc_core::Error {
                            code: jsonrpc_core::ErrorCode::ParseError,
                            message: format!(
                                "Failed to parse parameters: {}",
                                err
                            ),
                            data: None,
                        }) {
                        Ok(params) => params,
                        Err(err) => {
                            subscriber.reject(err).unwrap();
                            return;
                        }
                    };
                    debug!("{:#?}", signature_params);

                    std::thread::spawn(move || {
                        let sink = subscriber
                            .assign_id(SubscriptionId::Number(0))
                            .unwrap();

                        loop {
                            std::thread::sleep(
                                std::time::Duration::from_millis(1000),
                            );
                            let res = ResponseWithSubscriptionId {
                                result: Response {
                                    context: RpcResponseContext::new(0),
                                    value:
                                        RpcSignatureResult::ProcessedSignature(
                                            ProcessedSignatureResult {
                                                err: None,
                                            },
                                        ),
                                },
                                subscription: 0,
                            };

                            match sink.notify(res.into_params_map()) {
                                Ok(_) => {}
                                Err(_) => {
                                    debug!(
                                        "Subscription has ended, finishing."
                                    );
                                    break;
                                }
                            }
                        }
                    });
                },
            ),
            (
                "signatureUnsubscribe",
                |id: SubscriptionId,
                 _meta|
                 -> BoxFuture<jsonrpc_core::Result<Value>> {
                    debug!("Closing subscription {:?}", id);
                    Box::pin(futures::future::ready(Ok(Value::Bool(true))))
                },
            ),
        );
        self
    }

    #[allow(clippy::result_large_err)]
    pub fn start(self) -> jsonrpc_ws_server::Result<Server> {
        ServerBuilder::with_meta_extractor(
            self.io,
            |context: &RequestContext| Arc::new(Session::new(context.sender())),
        )
        .start(&self.config.socket)
    }

    pub fn spawn(config: RpcPubsubConfig) {
        let socket = format!("{:?}", config.socket());

        // NOTE: using tokio task here results in service not listening
        std::thread::spawn(move || {
            RpcPubsubService::new(config)
                .add_signature_subscribe()
                .start()
                .unwrap()
                .wait()
                .unwrap();
        });

        info!(
            "Launched PubSubService service with pid {} at {}",
            std::process::id(),
            socket,
        );
    }
}
