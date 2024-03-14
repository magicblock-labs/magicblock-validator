// NOTE: from rpc/src/rpc_service.rs
use std::{net::SocketAddr, sync::Arc};

use jsonrpc_core::MetaIoHandler;
use jsonrpc_http_server::{
    hyper, AccessControlAllowOrigin, DomainsValidation, ServerBuilder,
};
use sleipnir_bank::bank::Bank;
use solana_perf::thread::renice_this_thread;

use crate::{
    json_rpc_request_processor::{JsonRpcConfig, JsonRpcRequestProcessor},
    utils::MAX_REQUEST_BODY_SIZE,
};

pub struct JsonRpcService;
impl JsonRpcService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(rpc_addr: SocketAddr, bank: Arc<Bank>) {
        let config = JsonRpcConfig::default();

        let max_request_body_size = config
            .max_request_body_size
            .unwrap_or(MAX_REQUEST_BODY_SIZE);

        let runtime = get_runtime(&config);

        let (request_processor, receiver) =
            JsonRpcRequestProcessor::new(bank, config);

        // TODO: inside thread_hdl (:498)
        let mut io = MetaIoHandler::default();

        let server = ServerBuilder::with_meta_extractor(
            io,
            move |_req: &hyper::Request<hyper::Body>| request_processor.clone(),
        )
        .event_loop_executor(runtime.handle().clone())
        .threads(1)
        .cors(DomainsValidation::AllowOnly(vec![
            AccessControlAllowOrigin::Any,
        ]))
        .cors_max_age(86400)
        // TODO:
        // .request_middleware(request_middleware)
        .max_request_body_size(max_request_body_size)
        .start_http(&rpc_addr);
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
