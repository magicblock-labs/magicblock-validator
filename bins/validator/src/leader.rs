use std::{
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};

use aperture::Aperture;
use chainlink::{
    ProdChainlink,
    errors::ChainlinkError,
    remote_account_provider::{Endpoints, config::RemoteAccountProviderConfig},
};
use committor::{CommittorConfig, CommittorService, ComputeBudgetConfig, DEFAULT_ACTIONS_TIMEOUT};
use config::LeaderParams;
use engine::Engine;
use metrics::MetricsService;
use nucleus::{
    metrics::EventTimer,
    shutdown::{Service, ShutdownManager, ShutdownReason},
};
use program::{init_magic_sys, validator::init_validator_authority};
use replicator::ReplicationDispatcher;
use runtime::keeper_builder;
use services::{
    actions_callback_service::ActionsCallbackService,
    undelegation_request_service::UndelegationRequestService,
};
use solana_commitment_config::CommitmentConfig;
use solana_native_token::LAMPORTS_PER_SOL;
use solana_program::{rent::Rent, sysvar};
use solana_pubkey::Pubkey;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_signer::Signer;
use task_scheduler::TaskSchedulerService;
use tracing::*;

use crate::{
    errors::{ApiError, ApiResult},
    ledger,
    magic_sys_adapter::MagicSysAdapter,
};

// -----------------
// Leader
// -----------------
pub(crate) struct Leader {
    config: LeaderParams,
    engine: Engine,
    chainlink: Arc<ProdChainlink>,
    shutdown: ShutdownManager,
    committor: CommittorService,
    undelegation_request_service: UndelegationRequestService,
    metrics: MetricsService,
    task_scheduler: TaskSchedulerService,
}

pub(crate) struct RunningLeader {
    chainlink: Arc<ProdChainlink>,
    shutdown: ShutdownManager,
}

impl Leader {
    // -----------------
    // Initialization
    // -----------------
    #[instrument(skip_all, fields(last_slot = field::Empty))]
    pub(crate) async fn try_from_config(config: LeaderParams) -> ApiResult<Self> {
        let mut timer = EventTimer::new("init");

        let engine_ledger = config.engine.ledger.directory.clone();
        init_validator_authority(config.engine.authority.local.clone());

        let (ledger, _) = ledger::init(&engine_ledger, &config.ledger)?;
        let ledger = Arc::new(ledger);
        timer.record("Deprecated ledger initialized");

        let mut shutdown = ShutdownManager::default();
        let mut builder = keeper_builder(&config.engine, &config.programs)?;
        builder.rent = Self::fetch_rent_from_base_chain(config.rpc_url()).await?;
        timer.record("Keeper runtime configured");
        let engine = Engine::new(builder, None, &mut shutdown).await?;
        timer.record("Engine initialized");

        let allowed = config
            .engine
            .replication
            .allowed_followers
            .iter()
            .map(|follower| follower.0)
            .collect::<Vec<_>>()
            .into();
        let replication = ReplicationDispatcher::spawn(
            config.engine.replication.bind_address.0,
            engine.clone(),
            allowed,
            &mut shutdown,
        )
        .await;
        if let Err(error) = replication {
            let _ = shutdown.terminate().await;
            return Err(error.into());
        }
        timer.record("Replication dispatcher started");

        let shared_chain_slot = Arc::<AtomicU64>::default();

        let chainlink =
            Arc::new(Self::init_chainlink(&config, &engine, shared_chain_slot.clone()).await?);
        timer.record("Chainlink initialized");

        let committor =
            Self::init_committor(&config, &engine, chainlink.clone(), shared_chain_slot);
        timer.record("Committor service initialized");
        init_magic_sys(Arc::new(MagicSysAdapter::new(
            tokio::runtime::Handle::current(),
            committor.commit_id_provider(),
        )));
        timer.record("Magic syscall adapter initialized");

        let metrics = MetricsService::bind(config.metrics.address.0)
            .await
            .map_err(ApiError::FailedToStartMetricsService)?;
        timer.record("Metrics service bound");

        let undelegation_request_service = UndelegationRequestService::new(
            chainlink.clone(),
            engine.clone(),
            config.chainlink.undelegation_request_poll_interval,
        );
        let mut rpc_shutdown = shutdown.handle(Service::Rpc);
        let rpc = Aperture::bind(
            &config.aperture,
            engine.clone(),
            chainlink.clone(),
            ledger.clone(),
            config.engine.blockstore.blocktime,
            rpc_shutdown.child(),
        )
        .await?;
        timer.record("RPC service initialized");
        tokio::spawn(async move {
            rpc.run().await;
            let reason = if rpc_shutdown.requested() {
                ShutdownReason::Signalled
            } else {
                ShutdownReason::Unexpected
            };
            rpc_shutdown.terminate(reason);
        });

        debug!("Initializing task scheduler");
        let task_scheduler = TaskSchedulerService::new(
            engine.clone(),
            config.aperture.listen.http(),
            config.engine.blockstore.blocktime,
        )?;
        timer.record("Task scheduler initialized");

        Ok(Self {
            config,
            engine,
            chainlink,
            shutdown,
            committor,
            undelegation_request_service,
            metrics,
            task_scheduler,
        })
    }

    fn init_committor(
        config: &LeaderParams,
        engine: &Engine,
        chainlink: Arc<ProdChainlink>,
        chain_slot: Arc<AtomicU64>,
    ) -> CommittorService {
        let er_rpc = Arc::new(RpcClient::new(config.aperture.listen.http()));
        let callbacks = Arc::new(ActionsCallbackService::new(
            er_rpc.clone(),
            config.engine.authority.local.insecure_clone(),
            engine.clone(),
        ));
        CommittorService::new(
            config.engine.authority.local.insecure_clone(),
            CommittorConfig {
                rpc_uri: config.rpc_url().to_owned(),
                commitment: CommitmentConfig::confirmed(),
                websocket_uri: config.websocket_urls().next().map(ToOwned::to_owned),
                compute_budget_config: ComputeBudgetConfig::new(config.commit.compute_unit_price),
                actions_timeout: DEFAULT_ACTIONS_TIMEOUT,
                slot_interval: config.engine.blockstore.blocktime,
            },
            chain_slot,
            engine.clone(),
            chainlink,
            er_rpc,
            callbacks,
        )
    }

    #[instrument(skip_all)]
    async fn init_chainlink(
        config: &LeaderParams,
        engine: &Engine,
        chain_slot: Arc<AtomicU64>,
    ) -> ApiResult<ProdChainlink> {
        let endpoints =
            Endpoints::try_from(config.remotes.as_slice()).map_err(ChainlinkError::from)?;

        let provider_config = RemoteAccountProviderConfig::default()
            .with_resubscription_delay(config.chainlink.resubscription_delay)
            .map_err(ChainlinkError::from)?
            .with_grpc(config.grpc.clone());
        ProdChainlink::try_new_from_endpoints(
            &endpoints,
            CommitmentConfig::confirmed(),
            engine.clone(),
            config.engine.authority.local.insecure_clone(),
            provider_config,
            &config.chainlink,
            chain_slot,
        )
        .await
        .map_err(ApiError::from)
    }

    // -----------------
    // Start/Stop
    // -----------------
    async fn fetch_rent_from_base_chain(rpc_url: &str) -> ApiResult<Rent> {
        let account =
            RpcClient::new_with_commitment(rpc_url.to_owned(), CommitmentConfig::confirmed())
                .get_account(&sysvar::rent::ID)
                .await
                .map_err(|err| ApiError::FailedToSyncBaseChainRent(err.to_string()))?;
        let rent = bincode::deserialize(&account.data)
            .map_err(|err| ApiError::FailedToSyncBaseChainRent(err.to_string()))?;
        info!(?rent, "Fetched rent parameters from base chain");
        Ok(rent)
    }

    async fn ensure_validator_funded_on_chain(rpc_url: String, identity: Pubkey) -> ApiResult<()> {
        // NOTE: 5 SOL seems reasonable, but we may require a different amount in the future
        const MIN_BALANCE_SOL: u64 = 5;

        let lamports = RpcClient::new_with_commitment(rpc_url, CommitmentConfig::confirmed())
            .get_balance(&identity)
            .await
            .map_err(|err| {
                ApiError::FailedToObtainValidatorOnChainBalance(identity, Box::new(err))
            })?;
        if lamports < MIN_BALANCE_SOL * LAMPORTS_PER_SOL {
            Err(ApiError::ValidatorInsufficientlyFunded(
                identity,
                MIN_BALANCE_SOL,
            ))
        } else {
            Ok(())
        }
    }

    async fn ensure_magic_fee_vault_on_chain(engine: &Engine, rpc_url: String) -> ApiResult<()> {
        let validator_keypair = engine.signer().insecure_clone();
        let validator_pubkey = validator_keypair.pubkey();
        let vault_pubkey = dpi::legacy::pda::magic_fee_vault_pda_from_validator(&validator_pubkey);
        let delegation_record_pubkey =
            dpi::legacy::pda::delegation_record_pda_from_delegated_account(&vault_pubkey);

        let rpc = RpcClient::new_with_commitment(rpc_url, CommitmentConfig::confirmed());

        let accounts = rpc
            .get_multiple_accounts(&[vault_pubkey, delegation_record_pubkey])
            .await
            .map_err(|err| {
            ApiError::FailedToInitMagicFeeVault(validator_pubkey, Box::new(err))
        })?;

        let vault_exists = accounts[0].is_some();
        let delegation_record_exists = accounts[1].is_some();

        if !vault_exists {
            info!(%validator_pubkey, "Magic fee vault absent, initializing");
            let ix = dpi::legacy::instruction_builder::init_magic_fee_vault(
                validator_pubkey,
                validator_pubkey,
            );
            let blockhash = rpc.get_latest_blockhash().await.map_err(|err| {
                ApiError::FailedToInitMagicFeeVault(validator_pubkey, Box::new(err))
            })?;
            let tx = solana_transaction::Transaction::new_signed_with_payer(
                &[ix],
                Some(&validator_pubkey),
                &[&validator_keypair],
                blockhash,
            );
            rpc.send_and_confirm_transaction(&tx).await.map_err(|err| {
                match err.get_transaction_error() {
                    Some(tx_err) => {
                        ApiError::OnchainSetupTransactionRejected(validator_pubkey, tx_err)
                    }
                    None => ApiError::FailedToInitMagicFeeVault(validator_pubkey, Box::new(err)),
                }
            })?;
            info!(%validator_pubkey, "Magic fee vault initialized");
        } else {
            info!(%validator_pubkey, "Magic fee vault already exists, skipping init");
        }

        if !delegation_record_exists {
            info!(%validator_pubkey, "Magic fee vault not delegated, delegating");
            let ix = dpi::legacy::instruction_builder::delegate_magic_fee_vault(
                validator_pubkey,
                validator_pubkey,
            );
            let blockhash = rpc.get_latest_blockhash().await.map_err(|err| {
                ApiError::FailedToDelegateMagicFeeVault(validator_pubkey, Box::new(err))
            })?;
            let tx = solana_transaction::Transaction::new_signed_with_payer(
                &[ix],
                Some(&validator_pubkey),
                &[&validator_keypair],
                blockhash,
            );
            rpc.send_and_confirm_transaction(&tx).await.map_err(|err| {
                match err.get_transaction_error() {
                    Some(tx_err) => {
                        ApiError::OnchainSetupTransactionRejected(validator_pubkey, tx_err)
                    }
                    None => {
                        ApiError::FailedToDelegateMagicFeeVault(validator_pubkey, Box::new(err))
                    }
                }
            })?;
            info!(%validator_pubkey, "Magic fee vault delegated");
        } else {
            info!(%validator_pubkey, "Magic fee vault already delegated, skipping");
        }

        Ok(())
    }

    /// Retries a transient on-chain setup failure with backoff; definitive
    /// outcomes like insufficient funds surface immediately.
    async fn with_onchain_setup_retries<F, Fut>(step: &str, op: F) -> ApiResult<()>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = ApiResult<()>>,
    {
        const MAX_ATTEMPTS: u32 = 5;
        let mut delay = Duration::from_secs(2);
        let mut attempt = 0;
        loop {
            attempt += 1;
            match op().await {
                Ok(()) => return Ok(()),
                Err(
                    err @ (ApiError::ValidatorInsufficientlyFunded(..)
                    | ApiError::OnchainSetupTransactionRejected(..)),
                ) => return Err(err),
                Err(err) if attempt < MAX_ATTEMPTS => {
                    warn!(
                        step,
                        attempt,
                        max_attempts = MAX_ATTEMPTS,
                        retry_in_secs = delay.as_secs(),
                        error = ?err,
                        "On-chain setup step failed; retrying"
                    );
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn spawn_primary_onchain_setup(&mut self) {
        let engine = self.engine.clone();
        let rpc_url = self.config.rpc_url().to_owned();
        let identity = self.engine.authority();

        let mut shutdown = self.shutdown.handle(Service::OnchainSetup);
        // Ephemeral mode does a non-blocking startup balance check.
        tokio::spawn(async move {
            let setup = async move {
                let mut timer = EventTimer::new("onchain-setup");
                Self::with_onchain_setup_retries("ensure_funded_on_chain", || {
                    Leader::ensure_validator_funded_on_chain(rpc_url.clone(), identity)
                })
                .await?;
                timer.record("Validator balance checked");

                Self::with_onchain_setup_retries("ensure_magic_fee_vault_on_chain", || {
                    Leader::ensure_magic_fee_vault_on_chain(&engine, rpc_url.clone())
                })
                .await?;
                timer.record("Magic fee vault setup attempt completed");

                ApiResult::Ok(())
            };
            let result = tokio::select! {
                _ = shutdown.signalled() => {
                    debug!("On-chain setup cancelled by shutdown");
                    ApiResult::Ok(())
                }
                result = setup => result,
            };
            let reason = match result {
                Err(error) => ShutdownReason::Error(Box::new(error)),
                Ok(()) => {
                    shutdown.signalled().await;
                    ShutdownReason::Signalled
                }
            };
            shutdown.terminate(reason);
        });
    }

    #[instrument(skip(self))]
    pub(crate) fn start(mut self) -> RunningLeader {
        let mut timer = EventTimer::new("startup");
        self.spawn_primary_onchain_setup();

        let shutdown = self.shutdown.handle(Service::UndelegationRequests);
        tokio::spawn(self.undelegation_request_service.run(shutdown));
        timer.record("Undelegation request service started");

        let shutdown = self.shutdown.handle(Service::IntentExecution);
        tokio::spawn(self.committor.run(shutdown));
        timer.record("Intent execution service started");

        let shutdown = self.shutdown.handle(Service::TaskScheduler);
        tokio::spawn(self.task_scheduler.run(shutdown));
        timer.record("Task scheduler started");

        let shutdown = self.shutdown.handle(Service::Metrics);
        tokio::spawn(self.metrics.run(shutdown));
        timer.record("Metrics service started");

        RunningLeader {
            chainlink: self.chainlink,
            shutdown: self.shutdown,
        }
    }
}

impl RunningLeader {
    #[instrument(skip(self))]
    pub(crate) async fn wait(mut self) -> ShutdownReason {
        let reason = self.shutdown.wait().await;
        self.chainlink.shutdown().await;
        reason.combine(self.shutdown.terminate().await)
    }
}
