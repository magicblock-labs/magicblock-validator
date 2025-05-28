use std::{
    net::SocketAddr,
    sync::{atomic::AtomicBool, Arc, RwLock},
    thread::{self, JoinHandle},
};

use jsonrpc_core::MetaIoHandler;
use jsonrpc_http_server::{
    hyper, AccessControlAllowOrigin, CloseHandle, DomainsValidation,
    ServerBuilder,
};
// NOTE: from rpc/src/rpc_service.rs
use log::*;
use magicblock_accounts::AccountsManager;
use magicblock_bank::bank::Bank;
use magicblock_ledger::Ledger;
use solana_perf::thread::renice_this_thread;
use solana_sdk::{hash::Hash, signature::Keypair};

use crate::{
    handlers::{
        accounts::AccountsDataImpl, accounts_scan::AccountsScanImpl,
        bank_data::BankDataImpl, full::FullImpl, minimal::MinimalImpl,
    },
    json_rpc_request_processor::{JsonRpcConfig, JsonRpcRequestProcessor},
    rpc_health::RpcHealth,
    rpc_request_middleware::RpcRequestMiddleware,
    traits::{
        rpc_accounts::AccountsData, rpc_accounts_scan::AccountsScan,
        rpc_bank_data::BankData, rpc_full::Full, rpc_minimal::Minimal,
    },
    utils::MAX_REQUEST_BODY_SIZE,
};

pub struct JsonRpcService {
    rpc_addr: SocketAddr,
    rpc_niceness_adj: i8,
    request_processor: JsonRpcRequestProcessor,
    startup_verification_complete: Arc<AtomicBool>,
    max_request_body_size: usize,
    rpc_thread_handle: RwLock<Option<JoinHandle<()>>>,
    close_handle: Arc<RwLock<Option<CloseHandle>>>,
}

impl JsonRpcService {
    pub fn try_init(
        bank: Arc<Bank>,
        ledger: Arc<Ledger>,
        faucet_keypair: Keypair,
        genesis_hash: Hash,
        accounts_manager: Arc<AccountsManager>,
        config: JsonRpcConfig,
    ) -> Result<Self, String> {
        let rpc_addr = config
            .rpc_socket_addr
            .ok_or_else(|| "JSON RPC socket required".to_string())?;

        let max_request_body_size = config
            .max_request_body_size
            .unwrap_or(MAX_REQUEST_BODY_SIZE);

        let rpc_niceness_adj = config.rpc_niceness_adj;

        let startup_verification_complete =
            Arc::clone(bank.get_startup_verification_complete());
        let health = RpcHealth::new(startup_verification_complete.clone());

        let request_processor = JsonRpcRequestProcessor::new(
            bank,
            ledger,
            health.clone(),
            faucet_keypair,
            genesis_hash,
            accounts_manager,
            config,
        );

        Ok(Self {
            rpc_addr,
            rpc_niceness_adj,
            max_request_body_size,
            request_processor,
            startup_verification_complete,
            rpc_thread_handle: Default::default(),
            close_handle: Default::default(),
        })
    }

    pub fn start(&self) -> Result<(), String> {
        if self.close_handle.read().unwrap().is_some() {
            return Err("JSON RPC service already running".to_string());
        }

        let rpc_niceness_adj = self.rpc_niceness_adj;
        let startup_verification_complete =
            self.startup_verification_complete.clone();
        let request_processor = self.request_processor.clone();
        let rpc_addr = self.rpc_addr;
        let max_request_body_size = self.max_request_body_size;

        let close_handle_rc = self.close_handle.clone();
        let handle = tokio::runtime::Handle::current();
        let thread_handle = thread::Builder::new()
            .name("solJsonRpcSvc".to_string())
            .spawn(move || {
                renice_this_thread(rpc_niceness_adj).unwrap();

                let mut io = MetaIoHandler::default();

                io.extend_with(AccountsDataImpl.to_delegate());
                io.extend_with(AccountsScanImpl.to_delegate());
                io.extend_with(FullImpl.to_delegate());
                io.extend_with(BankDataImpl.to_delegate());
                io.extend_with(MinimalImpl.to_delegate());

                let health = RpcHealth::new(startup_verification_complete);
                let request_middleware = RpcRequestMiddleware::new(health);

                let server = ServerBuilder::with_meta_extractor(
                    io,
                    move |_req: &hyper::Request<hyper::Body>| {
                       request_processor.clone()
                    },
                )
                .event_loop_executor(handle)
                .cors(DomainsValidation::AllowOnly(vec![
                    AccessControlAllowOrigin::Any,
                ]))
                .cors_max_age(86400)
                .request_middleware(request_middleware)
                .max_request_body_size(max_request_body_size)
                .start_http(&rpc_addr);


                match server {
                    Err(e) => {
                        error!(
                            "JSON RPC service unavailable error: {:?}. \n\
                            Also, check that port {} is not already in use by another application",
                            e,
                            rpc_addr.port()
                        );
                    }
                    Ok(server) => {
                        let close_handle = server.close_handle().clone();
                        close_handle_rc
                            .write()
                            .unwrap()
                            .replace(close_handle);
                        server.wait();
                    }
                }
            })
            .unwrap();

        self.rpc_thread_handle
            .write()
            .unwrap()
            .replace(thread_handle);

        Ok(())
    }

    pub fn close(&self) {
        if let Some(close_handle) = self.close_handle.write().unwrap().take() {
            close_handle.close();
        }
    }

    pub fn join(&self) -> Result<(), String> {
        self.rpc_thread_handle
            .write()
            .unwrap()
            .take()
            .map(|x| x.join())
            .unwrap_or(Ok(()))
            .map_err(|err| format!("{:?}", err))
    }

    pub fn rpc_addr(&self) -> &SocketAddr {
        &self.rpc_addr
    }
}
