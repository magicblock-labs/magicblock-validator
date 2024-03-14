use crossbeam_channel::unbounded;
// NOTE: from rpc/src/rpc_service.rs
use log::*;
use std::{
    net::SocketAddr,
    sync::Arc,
    thread::{self, JoinHandle},
};

use jsonrpc_core::MetaIoHandler;
use jsonrpc_http_server::{
    hyper, AccessControlAllowOrigin, CloseHandle, DomainsValidation,
    ServerBuilder,
};
use sleipnir_bank::bank::Bank;
use solana_perf::thread::renice_this_thread;

use crate::{
    accounts::AccountsDataImpl,
    json_rpc_request_processor::{JsonRpcConfig, JsonRpcRequestProcessor},
    rpc_health::RpcHealth,
    rpc_request_middleware::RpcRequestMiddleware,
    traits::rpc_accounts::AccountsData,
    utils::MAX_REQUEST_BODY_SIZE,
};

pub struct JsonRpcService {
    thread_hdl: JoinHandle<()>,
    close_handle: Option<CloseHandle>,
}

impl JsonRpcService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(rpc_addr: SocketAddr, bank: Arc<Bank>) -> Result<Self, String> {
        let config = JsonRpcConfig::default();

        let max_request_body_size = config
            .max_request_body_size
            .unwrap_or(MAX_REQUEST_BODY_SIZE);

        let runtime = get_runtime(&config);
        let rpc_niceness_adj = config.rpc_niceness_adj;

        let startup_verification_complete =
            Arc::clone(bank.get_startup_verification_complete());
        let health = Arc::new(RpcHealth::new(startup_verification_complete));

        let (request_processor, _receiver) =
            JsonRpcRequestProcessor::new(bank, config, health.clone());

        let (close_handle_sender, close_handle_receiver) = unbounded();
        let thread_hdl = thread::Builder::new()
            .name("solJsonRpcSvc".to_string())
            .spawn(move || {
                renice_this_thread(rpc_niceness_adj).unwrap();

                let mut io = MetaIoHandler::default();

                io.extend_with(AccountsDataImpl.to_delegate());

                // io.extend_with(rpc_minimal::MinimalImpl.todelegate());
                // TODO: full_api :506

                // NOTE: left out obsolete_v1_7_api
                let request_middleware = RpcRequestMiddleware::new(health.clone());

                let server = ServerBuilder::with_meta_extractor(
                    io,
                    move |_req: &hyper::Request<hyper::Body>| {
                        request_processor.clone()
                    },
                )
                .event_loop_executor(runtime.handle().clone())
                .threads(1)
                .cors(DomainsValidation::AllowOnly(vec![
                    AccessControlAllowOrigin::Any,
                ]))
                .cors_max_age(86400)
                .request_middleware(request_middleware)
                .max_request_body_size(max_request_body_size)
                .start_http(&rpc_addr);

                match server {
                    Err(e) => {
                        warn!(
                            "JSON RPC service unavailable error: {:?}. \n\
                            Also, check that port {} is not already in use by another application",
                            e,
                            rpc_addr.port()
                        );
                        close_handle_sender.send(Err(e.to_string())).unwrap();
                    }
                    Ok(server) => {
                        server.wait();
                    }
                }
            })
            .unwrap();

        let close_handle = close_handle_receiver.recv().unwrap()?;

        // NOTE: left out registering close_handle.close with validator_exit :558

        Ok(Self {
            thread_hdl,
            close_handle: Some(close_handle),
        })
    }

    pub fn exit(&mut self) {
        if let Some(c) = self.close_handle.take() {
            c.close()
        }
    }

    pub fn join(mut self) -> thread::Result<()> {
        self.exit();
        self.thread_hdl.join()
    }
}

fn get_runtime(config: &JsonRpcConfig) -> Arc<tokio::runtime::Runtime> {
    let rpc_threads = 1.max(config.rpc_threads);
    let rpc_niceness_adj = config.rpc_niceness_adj;

    // Comment from Solana implementation:
    // sadly, some parts of our current rpc implemention block the jsonrpc's
    // _socket-listening_ event loop for too long, due to (blocking) long IO or intesive CPU,
    // causing no further processing of incoming requests and ultimatily innocent clients timing-out.
    // So create a (shared) multi-threaded event_loop for jsonrpc and set its .threads() to 1,
    // so that we avoid the single-threaded event loops from being created automatically by
    // jsonrpc for threads when .threads(N > 1) is given.
    Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(rpc_threads)
            .on_thread_start(move || {
                renice_this_thread(rpc_niceness_adj).unwrap()
            })
            .thread_name("solRpcEl")
            .enable_all()
            .build()
            .expect("Runtime"),
    )
}
