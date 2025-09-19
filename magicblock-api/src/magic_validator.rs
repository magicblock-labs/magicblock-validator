use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use conjunto_transwise::RpcProviderConfig;
use log::*;
use magicblock_account_cloner::chainext::ChainlinkCloner;
use magicblock_accounts::{
    scheduled_commits_processor::ScheduledCommitsProcessorImpl,
    utils::try_rpc_cluster_from_cluster, ScheduledCommitsProcessor,
};
use magicblock_accounts_db::AccountsDb;
use magicblock_aperture::{
    state::{NodeContext, SharedState},
    JsonRpcServer,
};
use magicblock_chainlink::{
    config::ChainlinkConfig,
    remote_account_provider::{
        chain_pubsub_client::ChainPubsubClientImpl,
        chain_rpc_client::ChainRpcClientImpl,
    },
    submux::SubMuxClient,
    Chainlink,
};
use magicblock_committor_service::{
    config::ChainConfig, BaseIntentCommittor, CommittorService,
    ComputeBudgetConfig,
};
use magicblock_config::{
    EphemeralConfig, LedgerConfig, LedgerResumeStrategy, LifecycleMode,
    ProgramConfig,
};
use magicblock_core::{
    link::{
        blocks::BlockUpdateTx, link, transactions::TransactionSchedulerHandle,
    },
    Slot,
};
use magicblock_ledger::{
    blockstore_processor::process_ledger,
    ledger_truncator::{LedgerTruncator, DEFAULT_TRUNCATION_TIME_INTERVAL},
    LatestBlock, Ledger,
};
use magicblock_metrics::MetricsService;
use magicblock_processor::{
    build_svm_env,
    scheduler::{state::TransactionSchedulerState, TransactionScheduler},
};
use magicblock_program::{
    init_persister,
    validator::{self, validator_authority},
    TransactionScheduler as ActionTransactionScheduler,
};
use magicblock_validator_admin::claim_fees::ClaimFeesTask;
use mdp::state::{
    features::FeaturesSet,
    record::{CountryCode, ErRecord},
    status::ErStatus,
    version::v0::RecordV0,
};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::{CommitmentConfig, CommitmentLevel},
    native_token::LAMPORTS_PER_SOL,
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::{
    domain_registry_manager::DomainRegistryManager,
    errors::{ApiError, ApiResult},
    external_config::{cluster_from_remote, try_convert_accounts_config},
    fund_account::{
        fund_magic_context, funded_faucet, init_validator_identity,
    },
    genesis_utils::{create_genesis_config_with_leader, GenesisConfigInfo},
    ledger::{
        self, read_validator_keypair_from_ledger,
        write_validator_keypair_to_ledger,
    },
    slot::advance_slot_and_update_ledger,
    tickers::{init_slot_ticker, init_system_metrics_ticker},
};

type ChainlinkImpl = Chainlink<
    ChainRpcClientImpl,
    SubMuxClient<ChainPubsubClientImpl>,
    AccountsDb,
    ChainlinkCloner,
>;

// -----------------
// MagicValidatorConfig
// -----------------
#[derive(Default)]
pub struct MagicValidatorConfig {
    pub validator_config: EphemeralConfig,
}

impl std::fmt::Debug for MagicValidatorConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MagicValidatorConfig")
            .field("validator_config", &self.validator_config)
            .finish()
    }
}

// -----------------
// MagicValidator
// -----------------
pub struct MagicValidator {
    config: EphemeralConfig,
    exit: Arc<AtomicBool>,
    token: CancellationToken,
    accountsdb: Arc<AccountsDb>,
    ledger: Arc<Ledger>,
    ledger_truncator: LedgerTruncator,
    slot_ticker: Option<tokio::task::JoinHandle<()>>,
    committor_service: Option<Arc<CommittorService>>,
    scheduled_commits_processor:
        Option<Arc<ScheduledCommitsProcessorImpl<CommittorService>>>,
    rpc_handle: JoinHandle<()>,
    identity: Pubkey,
    transaction_scheduler: TransactionSchedulerHandle,
    block_udpate_tx: BlockUpdateTx,
    _metrics: Option<(MetricsService, tokio::task::JoinHandle<()>)>,
    claim_fees_task: ClaimFeesTask,
}

impl MagicValidator {
    // -----------------
    // Initialization
    // -----------------
    pub async fn try_from_config(
        config: MagicValidatorConfig,
        identity_keypair: Keypair,
    ) -> ApiResult<Self> {
        // TODO(thlorenz): this will need to be recreated on each start
        let token = CancellationToken::new();
        let config = config.validator_config;

        let validator_pubkey = identity_keypair.pubkey();
        let GenesisConfigInfo {
            genesis_config,
            validator_pubkey,
            ..
        } = create_genesis_config_with_leader(
            u64::MAX,
            &validator_pubkey,
            config.validator.base_fees,
        );

        let ledger_resume_strategy = &config.ledger.resume_strategy();

        let (ledger, last_slot) = Self::init_ledger(&config.ledger)?;
        info!("Latest ledger slot: {}", last_slot);

        Self::sync_validator_keypair_with_ledger(
            ledger.ledger_path(),
            &identity_keypair,
            ledger_resume_strategy,
            config.ledger.skip_keypair_match_check,
        )?;

        // SAFETY:
        // this code will never panic as the ledger_path always appends the
        // rocksdb directory to whatever path is preconfigured for the ledger,
        // see `Ledger::do_open`, thus this path will always have a parent
        let storage_path = ledger
            .ledger_path()
            .parent()
            .expect("ledger_path didn't have a parent, should never happen");

        let latest_block = ledger.latest_block().load();
        let accountsdb = AccountsDb::new(
            &config.accounts.db,
            storage_path,
            latest_block.slot,
        )?;
        for (pubkey, account) in genesis_config.accounts {
            accountsdb.insert_account(&pubkey, &account.into());
        }

        let exit = Arc::<AtomicBool>::default();
        let ledger_truncator = LedgerTruncator::new(
            ledger.clone(),
            DEFAULT_TRUNCATION_TIME_INTERVAL,
            config.ledger.size,
        );

        init_validator_identity(&accountsdb, &validator_pubkey);
        fund_magic_context(&accountsdb);
        let faucet_keypair =
            funded_faucet(&accountsdb, ledger.ledger_path().as_path())?;

        let metrics_config = &config.metrics;
        let accountsdb = Arc::new(accountsdb);

        let metrics = if metrics_config.enabled {
            let metrics_service =
                magicblock_metrics::try_start_metrics_service(
                    metrics_config.service.socket_addr(),
                    token.clone(),
                )
                .map_err(ApiError::FailedToStartMetricsService)?;

            let system_metrics_ticker = init_system_metrics_ticker(
                Duration::from_secs(
                    metrics_config.system_metrics_tick_interval_secs,
                ),
                &ledger,
                &accountsdb,
                token.clone(),
            );

            Some((metrics_service, system_metrics_ticker))
        } else {
            None
        };

        let (accounts_config, remote_rpc_config) =
            try_get_remote_accounts_and_rpc_config(&config.accounts)?;

        let (dispatch, validator_channels) = link();

        let committor_persist_path =
            storage_path.join("committor_service.sqlite");
        debug!(
            "Committor service persists to: {}",
            committor_persist_path.display()
        );

        // TODO(thlorenz): when we support lifecycle modes again, only start it when needed
        let committor_service = Some(Arc::new(CommittorService::try_start(
            identity_keypair.insecure_clone(),
            committor_persist_path,
            ChainConfig {
                rpc_uri: remote_rpc_config.url().to_string(),
                commitment: remote_rpc_config
                    .commitment()
                    .unwrap_or(CommitmentLevel::Confirmed),
                compute_budget_config: ComputeBudgetConfig::new(
                    accounts_config.commit_compute_unit_price,
                ),
            },
        )?));
        let chainlink = Self::init_chainlink(
            committor_service.clone(),
            &remote_rpc_config,
            &config,
            &dispatch.transaction_scheduler,
            &ledger.latest_block().clone(),
            &accountsdb,
            validator_pubkey,
            faucet_keypair.pubkey(),
        )
        .await?;

        validator::init_validator_authority(identity_keypair);

        let txn_scheduler_state = TransactionSchedulerState {
            accountsdb: accountsdb.clone(),
            ledger: ledger.clone(),
            transaction_status_tx: validator_channels.transaction_status,
            txn_to_process_rx: validator_channels.transaction_to_process,
            account_update_tx: validator_channels.account_update,
            environment: build_svm_env(&accountsdb, latest_block.blockhash, 0),
        };
        txn_scheduler_state
            .load_upgradeable_programs(&programs_to_load(&config.programs))
            .map_err(|err| {
                ApiError::FailedToLoadProgramsIntoBank(format!("{:?}", err))
            })?;

        // Faucet keypair is only used for airdrops, which are not allowed in
        // the Ephemeral mode by setting the faucet to None in node context
        // (used by the RPC implementation), we effectively disable airdrops
        let faucet = (config.accounts.lifecycle != LifecycleMode::Ephemeral)
            .then_some(faucet_keypair);
        let node_context = NodeContext {
            identity: validator_pubkey,
            faucet,
            base_fee: config.validator.base_fees.unwrap_or_default(),
            featureset: txn_scheduler_state.environment.feature_set.clone(),
        };
        let transaction_scheduler =
            TransactionScheduler::new(1, txn_scheduler_state);
        transaction_scheduler.spawn();

        let shared_state = SharedState::new(
            node_context,
            accountsdb.clone(),
            ledger.clone(),
            chainlink,
            config.validator.millis_per_slot,
        );
        let rpc = JsonRpcServer::new(
            &config.rpc,
            shared_state,
            &dispatch,
            token.clone(),
        )
        .await?;
        let rpc_handle = tokio::spawn(rpc.run());

        Ok(Self {
            accountsdb,
            config,
            exit,
            _metrics: metrics,
            // TODO: set during [Self::start]
            slot_ticker: None,
            committor_service,
            // TODO: @@@
            scheduled_commits_processor: None,
            token,
            ledger,
            ledger_truncator,
            claim_fees_task: ClaimFeesTask::new(),
            rpc_handle,
            identity: validator_pubkey,
            transaction_scheduler: dispatch.transaction_scheduler,
            block_udpate_tx: validator_channels.block_update,
        })
    }

    async fn init_chainlink(
        committor_service: Option<Arc<CommittorService>>,
        rpc_config: &RpcProviderConfig,
        config: &EphemeralConfig,
        transaction_scheduler: &TransactionSchedulerHandle,
        latest_block: &LatestBlock,
        accountsdb: &Arc<AccountsDb>,
        validator_pubkey: Pubkey,
        faucet_pubkey: Pubkey,
    ) -> ApiResult<ChainlinkImpl> {
        use magicblock_chainlink::remote_account_provider::Endpoint;
        let accounts = try_convert_accounts_config(&config.accounts).expect(
            "Failed to derive accounts config from provided magicblock config",
        );
        let endpoints = accounts
            .remote_cluster
            .ws_urls()
            .into_iter()
            .map(|pubsub_url| Endpoint {
                rpc_url: rpc_config.url().to_string(),
                pubsub_url,
            })
            .collect::<Vec<_>>();

        let cloner = ChainlinkCloner::new(
            committor_service,
            config.accounts.clone.clone(),
            transaction_scheduler.clone(),
            accountsdb.clone(),
            latest_block.clone(),
        );
        let cloner = Arc::new(cloner);
        let accounts_bank = accountsdb.clone();
        let config = ChainlinkConfig::default_with_lifecycle_mode(
            LifecycleMode::Ephemeral.into(),
        );
        let commitment_config = {
            let level = rpc_config
                .commitment()
                .unwrap_or(CommitmentLevel::Confirmed);
            CommitmentConfig { commitment: level }
        };
        // TODO @@@: remove this diagnostics hack later
        if std::env::var("CHAINLINK_OFFLINE").is_ok() {
            warn!("CHAINLINK_OFFLINE is set, Chainlink will not connect to any remote endpoints");
            return Ok(ChainlinkImpl::try_new(&accounts_bank, None)?);
        }
        Ok(ChainlinkImpl::try_new_from_endpoints(
            &endpoints,
            commitment_config,
            &accounts_bank,
            &cloner,
            validator_pubkey,
            faucet_pubkey,
            config,
        )
        .await?)
    }

    fn init_ledger(
        ledger_config: &LedgerConfig,
    ) -> ApiResult<(Arc<Ledger>, Slot)> {
        let ledger_path = Path::new(&ledger_config.path);
        let (ledger, last_slot) = ledger::init(ledger_path, ledger_config)?;
        let ledger_shared = Arc::new(ledger);
        init_persister(ledger_shared.clone());
        Ok((ledger_shared, last_slot))
    }

    fn sync_validator_keypair_with_ledger(
        ledger_path: &Path,
        validator_keypair: &Keypair,
        resume_strategy: &LedgerResumeStrategy,
        skip_keypair_match_check: bool,
    ) -> ApiResult<()> {
        if !resume_strategy.is_resuming() {
            write_validator_keypair_to_ledger(ledger_path, validator_keypair)?;
        } else if let Ok(ledger_validator_keypair) =
            read_validator_keypair_from_ledger(ledger_path)
        {
            if ledger_validator_keypair.ne(validator_keypair)
                && !skip_keypair_match_check
            {
                return Err(
                    ApiError::LedgerValidatorKeypairNotMatchingProvidedKeypair(
                        ledger_path.display().to_string(),
                        ledger_validator_keypair.pubkey().to_string(),
                    ),
                );
            }
        } else {
            write_validator_keypair_to_ledger(ledger_path, validator_keypair)?;
        }

        Ok(())
    }

    // -----------------
    // Start/Stop
    // -----------------
    async fn maybe_process_ledger(&self) -> ApiResult<()> {
        if !self.config.ledger.resume_strategy().is_replaying() {
            return Ok(());
        }
        // SOLANA only allows blockhash to be valid for 150 slot back in time,
        // considering that the average slot time on solana is 400ms, then:
        const SOLANA_VALID_BLOCKHASH_AGE: u64 = 150 * 400;
        // we have this number for our max blockhash age in slots, which correspond to 60 seconds
        let max_block_age =
            SOLANA_VALID_BLOCKHASH_AGE / self.config.validator.millis_per_slot;
        let slot_to_continue_at = process_ledger(
            &self.ledger,
            &self.accountsdb,
            self.transaction_scheduler.clone(),
            max_block_age,
        )
        .await?;

        // The transactions to schedule and accept account commits re-run when we
        // process the ledger, however we do not want to re-commit them.
        // Thus while the ledger is processed we don't yet run the machinery to handle
        // scheduled commits and we clear all scheduled commits before fully starting the
        // validator.
        let scheduled_commits =
            ActionTransactionScheduler::default().scheduled_actions_len();
        debug!(
            "Found {} scheduled commits while processing ledger, clearing them",
            scheduled_commits
        );
        ActionTransactionScheduler::default().clear_scheduled_actions();

        // We want the next transaction either due to hydrating of cloned accounts or
        // user request to be processed in the next slot such that it doesn't become
        // part of the last block found in the existing ledger which would be incorrect.
        let (update_ledger_result, _) = advance_slot_and_update_ledger(
            &self.accountsdb,
            &self.ledger,
            &self.block_udpate_tx,
        );
        if let Err(err) = update_ledger_result {
            return Err(err.into());
        }
        if self.accountsdb.slot() != slot_to_continue_at {
            return Err(
                ApiError::NextSlotAfterLedgerProcessingNotMatchingBankSlot(
                    slot_to_continue_at,
                    self.accountsdb.slot(),
                ),
            );
        }

        info!(
            "Processed ledger, validator continues at slot {}",
            slot_to_continue_at
        );

        Ok(())
    }

    async fn register_validator_on_chain(
        &self,
        fqdn: impl ToString,
    ) -> ApiResult<()> {
        let url = cluster_from_remote(&self.config.accounts.remote);
        let country_code =
            CountryCode::from(self.config.validator.country_code.alpha3());
        let validator_keypair = validator_authority();
        let validator_info = ErRecord::V0(RecordV0 {
            identity: validator_keypair.pubkey(),
            status: ErStatus::Active,
            block_time_ms: self.config.validator.millis_per_slot as u16,
            base_fee: self.config.validator.base_fees.unwrap_or(0) as u16,
            features: FeaturesSet::default(),
            load_average: 0, // not implemented
            country_code,
            addr: fqdn.to_string(),
        });

        DomainRegistryManager::handle_registration_static(
            url.url(),
            &validator_keypair,
            validator_info,
        )
        .map_err(|err| {
            ApiError::FailedToRegisterValidatorOnChain(format!("{:?}", err))
        })
    }

    fn unregister_validator_on_chain(&self) -> ApiResult<()> {
        let url = cluster_from_remote(&self.config.accounts.remote);
        let validator_keypair = validator_authority();

        DomainRegistryManager::handle_unregistration_static(
            url.url(),
            &validator_keypair,
        )
        .map_err(|err| {
            ApiError::FailedToUnregisterValidatorOnChain(format!("{err:#}"))
        })
    }

    async fn ensure_validator_funded_on_chain(&self) -> ApiResult<()> {
        // NOTE: 5 SOL seems reasonable, but we may require a different amount in the future
        const MIN_BALANCE_SOL: u64 = 5;
        let (_, remote_rpc_config) =
            try_get_remote_accounts_and_rpc_config(&self.config.accounts)?;

        let lamports = RpcClient::new_with_commitment(
            remote_rpc_config.url().to_string(),
            CommitmentConfig {
                commitment: remote_rpc_config
                    .commitment()
                    .unwrap_or(CommitmentLevel::Confirmed),
            },
        )
        .get_balance(&self.identity)
        .await
        .map_err(|err| {
            ApiError::FailedToObtainValidatorOnChainBalance(
                self.identity,
                err.to_string(),
            )
        })?;
        if lamports < MIN_BALANCE_SOL * LAMPORTS_PER_SOL {
            Err(ApiError::ValidatorInsufficientlyFunded(
                self.identity,
                MIN_BALANCE_SOL,
            ))
        } else {
            Ok(())
        }
    }

    pub async fn start(&mut self) -> ApiResult<()> {
        if matches!(self.config.accounts.lifecycle, LifecycleMode::Ephemeral) {
            self.ensure_validator_funded_on_chain().await?;
            if let Some(ref fqdn) = self.config.validator.fqdn {
                self.register_validator_on_chain(fqdn).await?;
            }
        }

        self.maybe_process_ledger().await?;

        self.claim_fees_task.start(self.config.clone());

        self.slot_ticker = Some(init_slot_ticker(
            self.accountsdb.clone(),
            &self.scheduled_commits_processor,
            self.ledger.clone(),
            Duration::from_millis(self.config.validator.millis_per_slot),
            self.transaction_scheduler.clone(),
            self.block_udpate_tx.clone(),
            self.exit.clone(),
        ));

        self.ledger_truncator.start();

        validator::finished_starting_up();
        Ok(())
    }

    /* TODO: @@@ properly remove
    fn start_remote_account_fetcher_worker(&mut self) {
        if let Some(mut remote_account_fetcher_worker) =
            self.remote_account_fetcher_worker.take()
        {
            let cancellation_token = self.token.clone();
            self.remote_account_fetcher_handle =
                Some(tokio::spawn(async move {
                    remote_account_fetcher_worker
                        .start_fetch_request_processing(cancellation_token)
                        .await;
                }));
        }
    }

    fn start_remote_account_updates_worker(&mut self) {
        if let Some(remote_account_updates_worker) =
            self.remote_account_updates_worker.take()
        {
            let cancellation_token = self.token.clone();
            self.remote_account_updates_handle =
                Some(tokio::spawn(async move {
                    remote_account_updates_worker
                        .start_monitoring_request_processing(cancellation_token)
                        .await
                }));
        }
    }

    async fn start_remote_account_cloner_worker(&mut self) -> ApiResult<()> {
        if let Some(remote_account_cloner_worker) =
            self.remote_account_cloner_worker.take()
        {
            if let Some(committor_service) = &self.committor_service {
                if self.config.accounts.clone.prepare_lookup_tables
                    == PrepareLookupTables::Always
                {
                    debug!("Reserving common pubkeys for committor service");
                    map_committor_request_result(
                        committor_service.reserve_common_pubkeys(),
                        committor_service.clone(),
                    )
                    .await?;
                }
            }

            let _ = remote_account_cloner_worker.hydrate().await.inspect_err(
                |err| {
                    error!("Failed to hydrate validator accounts: {:?}", err);
                },
            );
            info!("Validator hydration complete (bank hydrate, replay, account clone)");

            let cancellation_token = self.token.clone();
            self.remote_account_cloner_handle =
                Some(tokio::spawn(async move {
                    remote_account_cloner_worker
                        .start_clone_request_processing(cancellation_token)
                        .await
                }));
        }
        Ok(())
    }
    */

    pub async fn stop(mut self) {
        self.exit.store(true, Ordering::Relaxed);

        // Ordering is important here
        // Commitor service shall be stopped last
        self.token.cancel();
        if let Some(ref scheduled_commits_processor) =
            self.scheduled_commits_processor
        {
            scheduled_commits_processor.stop();
        }
        if let Some(ref committor_service) = self.committor_service {
            committor_service.stop();
        }

        self.ledger_truncator.stop();
        self.claim_fees_task.stop();

        if self.config.validator.fqdn.is_some()
            && matches!(
                self.config.accounts.lifecycle,
                LifecycleMode::Ephemeral
            )
        {
            if let Err(err) = self.unregister_validator_on_chain() {
                error!("Failed to unregister: {}", err)
            }
        }
        self.accountsdb.flush();

        // we have two memory mapped databases, flush them to disk before exitting
        if let Err(err) = self.ledger.shutdown(false) {
            error!("Failed to shutdown ledger: {:?}", err);
        }
        let _ = self.rpc_handle.await;
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }
}

fn programs_to_load(programs: &[ProgramConfig]) -> Vec<(Pubkey, String)> {
    programs
        .iter()
        .map(|program| (program.id, program.path.clone()))
        .collect()
}

fn try_get_remote_accounts_and_rpc_config(
    accounts: &magicblock_config::AccountsConfig,
) -> ApiResult<(magicblock_accounts::AccountsConfig, RpcProviderConfig)> {
    let accounts_config =
        try_convert_accounts_config(accounts).map_err(ApiError::ConfigError)?;
    let remote_rpc_config = RpcProviderConfig::new(
        try_rpc_cluster_from_cluster(&accounts_config.remote_cluster)?,
        Some(CommitmentLevel::Confirmed),
    );
    Ok((accounts_config, remote_rpc_config))
}
