use jsonrpc_ws_server::{RequestContext, Server, ServerBuilder};
use log::*;
use serde_json::Value;
use sleipnir_geyser_plugin::rpc::GeyserRpcService;
use std::{net::SocketAddr, sync::Arc};
use tokio::runtime::Builder;

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

use crate::{
    conversions::geyser_sub_for_transaction_signature,
    pubsub_types::{ResponseWithSubscriptionId, SignatureParams},
};

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
    geyser_rpc_service: Arc<GeyserRpcService>,
    config: RpcPubsubConfig,
    io: PubSubHandler<Arc<Session>>,
}

impl RpcPubsubService {
    pub fn new(
        config: RpcPubsubConfig,
        geyser_rpc_service: Arc<GeyserRpcService>,
    ) -> Self {
        let io = PubSubHandler::new(MetaIoHandler::default());
        Self {
            config,
            io,
            geyser_rpc_service,
        }
    }

    pub fn add_signature_subscribe(mut self) -> Self {
        let geyser_rpc_service = self.geyser_rpc_service.clone();
        self.io.add_subscription(
            "signatureNotification",
            (
                "signatureSubscribe",
                move |params: Params, _, subscriber: Subscriber| {
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

                    let sub = geyser_sub_for_transaction_signature(
                        signature_params.signature().to_string(),
                    );
                    debug!("{:#?}", sub);

                    let geyser_rpc_service = geyser_rpc_service.clone();

                    let rt = match Builder::new_multi_thread()
                        .thread_name("pubsubSignatureSubscribe")
                        .enable_all()
                        .build() {
                            Ok(rt) => rt,
                            Err(err) => {
                                error!(
                                    "Failed to create runtime for subscription: {:?}",
                                    err
                                );
                                subscriber
                                    .reject(jsonrpc_core::Error {
                                        code: jsonrpc_core::ErrorCode::InternalError,
                                        message: format!(
                                            "Failed to create runtime for subscription: {:?}",
                                            err
                                        ),
                                        data: None,
                                    })
                                    .unwrap();
                                return;
                            }
                        };
                    rt.block_on(async move {
                        let sub_id = match geyser_rpc_service.transaction_subscribe(sub)
                            {
                                Ok(id) => id,
                                Err(err) => {
                                    error!(
                                        "Failed to subscribe to signature: {:?}",
                                        err
                                    );
                                    subscriber
                                        .reject(jsonrpc_core::Error {
                                            code: jsonrpc_core::ErrorCode::InvalidRequest,
                                            message: format!("Could not convert to proper GRPC sub {:?}", err),
                                            data: None,
                                        })
                                        .unwrap();
                                    return;
                                }
                            };

                        let sink = subscriber
                            .assign_id(SubscriptionId::Number(sub_id))
                            .unwrap();

                        info!("Got sink");
                        loop {
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

                            info!("Sending response: {:?}", res);
                            match sink.notify(res.into_params_map()) {
                                Ok(_) => {
                                    tokio::time::sleep(
                                        tokio::time::Duration::from_millis(1000),
                                    ).await;
                                }
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

    pub fn spawn(
        config: RpcPubsubConfig,
        geyser_rpc_service: Arc<GeyserRpcService>,
    ) {
        let socket = format!("{:?}", config.socket());

        // NOTE: using tokio task here results in service not listening
        std::thread::spawn(move || {
            RpcPubsubService::new(config, geyser_rpc_service)
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
