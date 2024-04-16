use std::{net::SocketAddr, str::FromStr, sync::Arc};

use jsonrpc_core::{futures, BoxFuture, MetaIoHandler, Params};
use jsonrpc_pubsub::{
    PubSubHandler, Session, Sink, Subscriber, SubscriptionId,
};
use jsonrpc_ws_server::{RequestContext, Server, ServerBuilder};
use log::*;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sleipnir_bank::bank::Bank;
use sleipnir_geyser_plugin::rpc::GeyserRpcService;
use sleipnir_rpc_client_api::{
    config::UiAccountEncoding,
    response::{ProcessedSignatureResult, RpcSignatureResult},
};
use solana_sdk::{
    pubkey::Pubkey, rpc_port::DEFAULT_RPC_PUBSUB_PORT, signature::Signature,
    transaction::TransactionError,
};
use tokio::{
    runtime::{Builder, Runtime},
    sync::broadcast,
};

use crate::{
    conversions::{
        geyser_sub_for_account, geyser_sub_for_slot_update,
        geyser_sub_for_transaction_signature, slot_from_update,
        subscribe_update_into_slot_response,
        subscribe_update_try_into_ui_account,
    },
    pubsub_types::{
        AccountParams, ReponseNoContextWithSubscriptionId,
        ResponseWithSubscriptionId, SignatureParams,
    },
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
    unsub_tx: Arc<broadcast::Sender<u64>>,
    bank: Arc<Bank>,
}

impl RpcPubsubService {
    pub fn new(
        config: RpcPubsubConfig,
        geyser_rpc_service: Arc<GeyserRpcService>,
        bank: Arc<Bank>,
    ) -> Self {
        let io = PubSubHandler::new(MetaIoHandler::default());
        let (unsub_tx, _) = broadcast::channel(100);
        let unsub_tx = Arc::new(unsub_tx);
        Self {
            config,
            io,
            geyser_rpc_service,
            unsub_tx,
            bank,
        }
    }

    pub fn add_signature_subscribe(mut self) -> Self {
        let subscribe = {
            let geyser_rpc_service = self.geyser_rpc_service.clone();
            let unsub_tx = self.unsub_tx.clone();
            let bank = self.bank.clone();
            (
                "signatureSubscribe",
                move |params: Params, _, subscriber: Subscriber| {
                    let (subscriber, signature_params): (
                        Subscriber,
                        SignatureParams,
                    ) = match ensure_and_try_parse_params(subscriber, params) {
                        Some((subscriber, params)) => (subscriber, params),
                        None => {
                            return;
                        }
                    };
                    handle_signature_subscribe(
                        subscriber,
                        signature_params,
                        &geyser_rpc_service,
                        &bank,
                        unsub_tx.subscribe(),
                    );
                },
            )
        };

        let unsubscribe = {
            let unsub_tx = self.unsub_tx.clone();
            (
                "signatureUnsubscribe",
                move |id: SubscriptionId,
                 _meta|
                 -> BoxFuture<jsonrpc_core::Result<Value>> {
                    handle_unsubscribe(id, &unsub_tx, true)
                },
            )
        };

        self.io.add_subscription(
            "signatureNotification",
            subscribe,
            unsubscribe,
        );
        self
    }

    pub fn add_account_subscribe(mut self) -> Self {
        let subscribe = {
            let geyser_rpc_service = self.geyser_rpc_service.clone();
            let unsub_tx = self.unsub_tx.clone();
            (
                "accountSubscribe",
                move |params: Params, _, subscriber: Subscriber| {
                    let (subscriber, account_params): (
                        Subscriber,
                        AccountParams,
                    ) = match ensure_and_try_parse_params(subscriber, params) {
                        Some((subscriber, params)) => (subscriber, params),
                        None => {
                            return;
                        }
                    };

                    debug!("{:#?}", account_params);
                    handle_account_subscribe(
                        subscriber,
                        account_params,
                        &geyser_rpc_service,
                        unsub_tx.subscribe(),
                    );
                },
            )
        };
        let unsubscribe = {
            let unsub_tx = self.unsub_tx.clone();
            (
                "accountUnsubscribe",
                move |id: SubscriptionId,
                 _meta|
                 -> BoxFuture<jsonrpc_core::Result<Value>> {
                    handle_unsubscribe(id, &unsub_tx, false)
                },
            )
        };
        self.io
            .add_subscription("accountNotification", subscribe, unsubscribe);
        self
    }

    fn add_slot_subscribe(mut self) -> Self {
        let subscribe = {
            let geyser_rpc_service = self.geyser_rpc_service.clone();
            let unsub_tx = self.unsub_tx.clone();
            (
                "slotSubscribe",
                move |params: Params, _, subscriber: Subscriber| {
                    let subscriber =
                        match ensure_empty_params(subscriber, &params, true) {
                            Some(subscriber) => subscriber,
                            None => return,
                        };

                    handle_slot_subscribe(
                        subscriber,
                        &geyser_rpc_service,
                        unsub_tx.subscribe(),
                    );
                },
            )
        };
        let unsubscribe = {
            let unsub_tx = self.unsub_tx.clone();
            (
                "slotUnsubscribe",
                move |id: SubscriptionId,
                    _meta|
                    -> BoxFuture<jsonrpc_core::Result<Value>> {
                    handle_unsubscribe(id, &unsub_tx, false)
                },
            )
        };

        self.io
            .add_subscription("slotNotification", subscribe, unsubscribe);
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
        bank: Arc<Bank>,
    ) {
        let socket = format!("{:?}", config.socket());

        tokio::spawn(async move {
            RpcPubsubService::new(config, geyser_rpc_service, bank)
                .add_signature_subscribe()
                .add_account_subscribe()
                .add_slot_subscribe()
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

fn handle_unsubscribe(
    id: SubscriptionId,
    unsub_tx: &broadcast::Sender<u64>,
    oneshot_sub: bool,
) -> std::pin::Pin<
    Box<futures::prelude::future::Ready<Result<Value, jsonrpc_core::Error>>>,
> {
    match id {
        SubscriptionId::Number(id) => {
            let res = unsub_tx.send(id);
            if !oneshot_sub {
                let _ = res.map_err(|err| {
                    error!("Failed to send unsubscription signal: {:?}", err)
                });
            }
        }
        SubscriptionId::String(_) => {
            unreachable!("We only support subs with number id")
        }
    }
    Box::pin(futures::future::ready(Ok(Value::Bool(true))))
}

// -----------------
// Handlers
// -----------------
fn handle_account_subscribe(
    subscriber: Subscriber,
    account_params: AccountParams,
    geyser_rpc_service: &Arc<GeyserRpcService>,
    mut unsub_rx: broadcast::Receiver<u64>,
) {
    let geyser_rpc_service = geyser_rpc_service.clone();
    std::thread::spawn(move || {
        if let Some((rt, subscriber)) =
            try_create_subscription_runtime("accountSubRt", subscriber)
        {
            rt.block_on(async move {
                let address = account_params.pubkey();
                let sub = geyser_sub_for_account(address.to_string());

                let pubkey = match Pubkey::try_from(address) {
                    Ok(pubkey) => pubkey,
                    Err(err) => {
                        reject_internal_error(
                            subscriber,
                            "Invalid Pubkey",
                            Some(err),
                        );
                        return;
                    }
                };

                let (sub_id, mut rx) =
                    match geyser_rpc_service.accounts_subscribe(sub, &pubkey) {
                        Ok(res) => res,
                        Err(err) => {
                            reject_internal_error(
                                subscriber,
                                "Failed to subscribe to signature",
                                Some(err),
                            );
                            return;
                        }
                    };

                if let Some(sink) = assign_sub_id(subscriber, sub_id) {
                    loop {
                        tokio::select! {
                            val = rx.recv() => {
                                match val {
                                    Some(Ok(update)) => {
                                        let slot = slot_from_update(&update).unwrap_or(0);
                                        debug!("Received account update: {:?}", update);
                                        // Solana RPC also defaults to base58 but enforces a size limit in
                                        // that case which we aren't doing here
                                        let encoding = account_params.encoding().unwrap_or(UiAccountEncoding::Base58);
                                        let ui_account = match subscribe_update_try_into_ui_account(
                                            update,
                                            encoding,
                                            account_params.data_slice_config(),
                                        ) {
                                            Ok(Some(ui_account)) => ui_account,
                                            Ok(None) => {
                                                debug!("No account data in update, skipping.");
                                                continue;
                                            }
                                            Err(err) => {
                                                let msg = format!(
                                                    "Failed to convert update to UiAccount: {:?}",
                                                    err
                                                );
                                                let failed_to_notify = sink_notify_error(&sink, msg);
                                                if failed_to_notify {
                                                    break;
                                                }
                                                continue;
                                            }
                                        };
                                        let res = ResponseWithSubscriptionId::new(
                                            ui_account,
                                            slot,
                                            sub_id,
                                        );
                                        debug!("Sending response: {:?}", res);
                                        if let Err(err) = sink.notify(res.into_params_map())
                                        {
                                            debug!(
                                                "Subscription has ended, finishing {:?}.",
                                                err
                                            );
                                            break;
                                        }
                                    }
                                    Some(Err(status)) => {
                                        let failed_to_notify = sink_notify_error(&sink, format!(
                                            "Failed to receive signature update: {:?}",
                                            status
                                        ));
                                        if failed_to_notify {
                                            break;
                                        }
                                    }
                                    None => {
                                        debug!(
                                            "Underlying Subscription has ended, finishing."
                                        );
                                        break;
                                    }
                                }
                            }
                            val = unsub_rx.recv() => {
                                match val {
                                    Ok(id) if id == sub_id => {
                                        debug!("received account unsubscription request for: {:?}", id);
                                        break;
                                    },
                                    Err(err) => debug!("failed to receive account unsubscription signal: {:?}", err),
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            })
        }
    });
}

fn handle_signature_subscribe(
    subscriber: Subscriber,
    signature_params: SignatureParams,
    geyser_rpc_service: &Arc<GeyserRpcService>,
    bank: &Arc<Bank>,
    mut unsub_rx: broadcast::Receiver<u64>,
) {
    let geyser_rpc_service = geyser_rpc_service.clone();

    let sigstr = signature_params.signature();
    let sub = geyser_sub_for_transaction_signature(sigstr.to_string());

    let sig = match Signature::from_str(sigstr) {
        Ok(sig) => sig,
        Err(err) => {
            reject_internal_error(subscriber, "Invalid Signature", Some(err));
            return;
        }
    };

    let bank = bank.clone();
    std::thread::spawn(move || {
        if let Some((rt, subscriber)) =
            try_create_subscription_runtime("signatureSubRt", subscriber)
        {
            rt.block_on(async move {
                let (sub_id, mut rx) =
                    match geyser_rpc_service.transaction_subscribe(sub, &sig) {
                        Ok(res) => res,
                        Err(err) => {
                            reject_internal_error(
                                subscriber,
                                "Failed to subscribe to signature",
                                Some(err),
                            );
                            return;
                        }
                    };

                if let Some(sink) = assign_sub_id(subscriber, sub_id) {
                    if let Some((slot, res)) = bank.get_signature_status_slot(&sig) {
                        info!("Sending initial signature status: {} {:?}", slot, res);
                        sink_notify_transaction_result(&sink, slot, sub_id, res.err());
                    }
                    loop {
                        tokio::select! {
                            val = rx.recv() => {
                                match val {
                                    Some(Ok(update)) => {
                                        let slot = slot_from_update(&update).unwrap_or(0);
                                        sink_notify_transaction_result(&sink, slot, sub_id, None);
                                        // single notification subscription
                                        // see: https://solana.com/docs/rpc/websocket/signaturesubscribe
                                        break;
                                    }
                                    Some(Err(status)) => {
                                        let failed_to_notify = sink_notify_error(&sink, format!(
                                            "Failed to receive signature update: {:?}",
                                            status
                                        ));
                                        if failed_to_notify {
                                            break;
                                        }
                                    }
                                    None => {
                                        debug!(
                                            "Underlying Subscription has ended, finishing."
                                        );
                                        break;
                                    }
                                }
                            }
                            val = unsub_rx.recv() => {
                                match val {
                                    Ok(id) if id == sub_id => {
                                        debug!("Received signature unsubscription request for: {:?}", id);
                                        break;
                                    },
                                    Err(err) => debug!("Failed to receive signature unsubscription signal: {:?}", err),
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            });
        }
    });
}

fn handle_slot_subscribe(
    subscriber: Subscriber,
    geyser_rpc_service: &Arc<GeyserRpcService>,
    mut unsub_rx: broadcast::Receiver<u64>,
) {
    let geyser_rpc_service = geyser_rpc_service.clone();
    std::thread::spawn(move || {
        if let Some((rt, subscriber)) =
            try_create_subscription_runtime("slotSubRt", subscriber)
        {
            rt.block_on(async move {
                let sub = geyser_sub_for_slot_update();
                let (sub_id, mut rx) =
                    match geyser_rpc_service.slot_subscribe(sub) {
                        Ok(res) => res,
                        Err(err) => {
                            reject_internal_error(
                                subscriber,
                                "Failed to subscribe to slot",
                                Some(err),
                            );
                            return;
                        }
                    };

                if let Some(sink) = assign_sub_id(subscriber, sub_id) {
                    loop {
                        tokio::select! {
                            val = rx.recv() => {
                                match val {
                                    Some(Ok(update)) => {
                                        let slot_response = match subscribe_update_into_slot_response(
                                            update,
                                        ) {
                                            Some(slot_response) => slot_response,
                                            None => {
                                                debug!("No slot in update, skipping.");
                                                continue;
                                            }
                                        };
                                        let res = ReponseNoContextWithSubscriptionId::new(
                                            slot_response,
                                            sub_id,
                                        );
                                        trace!("Sending Slot update response: {:?}", res);
                                        if let Err(err) = sink.notify(res.into_params_map())
                                        {
                                            debug!(
                                                "Subscription has ended, finishing {:?}.",
                                                err
                                            );
                                            break;
                                        }
                                    }
                                    Some(Err(status)) => {
                                        let failed_to_notify = sink_notify_error(&sink, format!(
                                            "Failed to receive signature update: {:?}",
                                            status
                                        ));
                                        if failed_to_notify {
                                            break;
                                        }
                                    }
                                    None => {
                                        debug!(
                                            "Underlying Subscription has ended, finishing."
                                        );
                                        break;
                                    }
                                };
                            }
                            val = unsub_rx.recv() => {
                                match val {
                                    Ok(id) if id == sub_id => {
                                        debug!("Received slot unsubscription request for: {:?}", id);
                                        break;
                                    },
                                    Err(err) => debug!("Failed to receive slot unsubscription signal: {:?}", err),
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            })
        }
    });
}

// -----------------
// Helpers
// -----------------
fn try_create_subscription_runtime(
    name: &str,
    subscriber: Subscriber,
) -> Option<(Runtime, Subscriber)> {
    match Builder::new_multi_thread()
        .thread_name(name)
        .enable_all()
        .build()
    {
        Ok(rt) => Some((rt, subscriber)),
        Err(err) => {
            error!("Failed to create runtime for subscription: {:?}", err);
            reject_internal_error(
                subscriber,
                "Failed to create runtime for subscription",
                Some(err),
            );
            None
        }
    }
}

fn ensure_params(
    subscriber: Subscriber,
    params: &Params,
) -> Option<Subscriber> {
    if params == &Params::None {
        reject_parse_error(subscriber, "Missing parameters", None::<()>);
        None
    } else {
        Some(subscriber)
    }
}

fn ensure_empty_params(
    subscriber: Subscriber,
    params: &Params,
    warn: bool,
) -> Option<Subscriber> {
    if params == &Params::None {
        Some(subscriber)
    } else if warn {
        warn!("Parameters should be empty");
        Some(subscriber)
    } else {
        reject_parse_error(
            subscriber,
            "Parameters should be empty",
            None::<()>,
        );
        None
    }
}

fn try_parse_params<D: DeserializeOwned>(
    subscriber: Subscriber,
    params: Params,
) -> Option<(Subscriber, D)> {
    match params.parse() {
        Ok(params) => Some((subscriber, params)),
        Err(err) => {
            reject_parse_error(
                subscriber,
                "Failed to parse parameters",
                Some(err),
            );
            None
        }
    }
}

fn ensure_and_try_parse_params<D: DeserializeOwned>(
    subscriber: Subscriber,
    params: Params,
) -> Option<(Subscriber, D)> {
    ensure_params(subscriber, &params)
        .and_then(|subscriber| try_parse_params(subscriber, params))
}

fn reject_internal_error<T: std::fmt::Debug>(
    subscriber: Subscriber,
    msg: &str,
    err: Option<T>,
) {
    _reject_subscriber_error(
        subscriber,
        msg,
        err,
        jsonrpc_core::ErrorCode::InternalError,
    )
}

fn reject_parse_error<T: std::fmt::Debug>(
    subscriber: Subscriber,
    msg: &str,
    err: Option<T>,
) {
    _reject_subscriber_error(
        subscriber,
        msg,
        err,
        jsonrpc_core::ErrorCode::ParseError,
    )
}

fn _reject_subscriber_error<T: std::fmt::Debug>(
    subscriber: Subscriber,
    msg: &str,
    err: Option<T>,
    code: jsonrpc_core::ErrorCode,
) {
    let message = match err {
        Some(err) => format!("{msg}: {:?}", err),
        None => msg.to_string(),
    };
    if let Err(reject_err) = subscriber.reject(jsonrpc_core::Error {
        code,
        message,
        data: None,
    }) {
        error!("Failed to reject subscriber: {:?}", reject_err);
    };
}

/// Tries to notify the sink of the error.
/// Returns true if the sink could not be notified
fn sink_notify_error(sink: &Sink, msg: String) -> bool {
    error!("{}", msg);
    let map = {
        let mut map = serde_json::Map::new();
        map.insert("error".to_string(), Value::String(msg));
        map
    };

    if let Err(err) = sink.notify(Params::Map(map)) {
        debug!("Subscription has ended, finishing {:?}.", err);
        true
    } else {
        false
    }
}

fn sink_notify_transaction_result(
    sink: &Sink,
    slot: u64,
    sub_id: u64,
    err: Option<TransactionError>,
) {
    let res = ResponseWithSubscriptionId::new(
        RpcSignatureResult::ProcessedSignature(ProcessedSignatureResult {
            err,
        }),
        slot,
        sub_id,
    );
    if let Err(err) = sink.notify(res.into_params_map()) {
        debug!("Subscription has ended {:?}.", err);
    }
}

fn assign_sub_id(subscriber: Subscriber, sub_id: u64) -> Option<Sink> {
    match subscriber.assign_id(SubscriptionId::Number(sub_id)) {
        Ok(sink) => Some(sink),
        Err(err) => {
            error!("Failed to assign subscription id: {:?}", err);
            None
        }
    }
}
