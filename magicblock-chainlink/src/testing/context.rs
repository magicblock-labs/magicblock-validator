use std::{
    rc::Rc,
    sync::{Arc, atomic::AtomicU64},
    time::{Duration, Instant},
};

use engine::{
    Engine,
    testkit::{Pacing, TestEngine},
};
use keeper::testkit::{Dirs, keeper_builder};
use magicblock_aml::RiskService;
use magicblock_config::config::LifecycleMode;
use solana_account::{Account, AccountBuilder, AccountMode, AccountSharedData};
use solana_keypair::Keypair;
use solana_program::clock::Slot;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use tokio::sync::mpsc;
use tracing::trace;

use super::accounts::account_shared_with_owner_and_slot;
use crate::{
    AccountFetchContext, InnerChainlink,
    errors::ChainlinkResult,
    fetch_cloner::FetchCloner,
    remote_account_provider::{
        RemoteAccountProvider,
        chain_pubsub_client::mock::ChainPubsubClientMock,
        config::RemoteAccountProviderConfig,
    },
    testing::{
        accounts::account_shared_with_owner,
        deleg::add_delegation_record_for,
        rpc_client_mock::{ChainRpcClientMock, ChainRpcClientMockBuilder},
        utils::create_test_subscribed_accounts_with_config,
    },
};
pub type TestChainlink =
    InnerChainlink<ChainRpcClientMock, ChainPubsubClientMock>;

#[derive(Clone)]
pub struct TestContext {
    pub rpc_client: ChainRpcClientMock,
    pub pubsub_client: ChainPubsubClientMock,
    pub chainlink: Arc<TestChainlink>,
    pub test_engine: Rc<TestEngine>,
    pub bank: Engine,
    pub validator_pubkey: Pubkey,
}

impl TestContext {
    pub async fn init(slot: Slot) -> Self {
        Self::init_with_config_and_risk(
            slot,
            RemoteAccountProviderConfig::default_with_lifecycle_mode(
                LifecycleMode::Ephemeral,
            ),
            None,
            None,
        )
        .await
    }

    pub async fn init_with_risk_service(
        slot: Slot,
        risk_service: Option<Arc<RiskService>>,
    ) -> Self {
        Self::init_with_config_and_risk(
            slot,
            RemoteAccountProviderConfig::default_with_lifecycle_mode(
                LifecycleMode::Ephemeral,
            ),
            risk_service,
            None,
        )
        .await
    }

    pub async fn init_with_config(
        slot: Slot,
        config: RemoteAccountProviderConfig,
    ) -> Self {
        Self::init_with_config_and_risk(slot, config, None, None).await
    }

    pub async fn init_with_lru_capacity(slot: Slot, capacity: usize) -> Self {
        Self::init_with_config_and_risk(
            slot,
            RemoteAccountProviderConfig::default_with_lifecycle_mode(
                LifecycleMode::Ephemeral,
            ),
            None,
            Some(capacity),
        )
        .await
    }

    async fn init_with_config_and_risk(
        slot: Slot,
        config: RemoteAccountProviderConfig,
        risk_service: Option<Arc<RiskService>>,
        lru_capacity: Option<usize>,
    ) -> Self {
        super::init_logger();
        let (rpc_client, pubsub_client) = {
            let rpc_client =
                ChainRpcClientMockBuilder::new().slot(slot).build();
            let (updates_sndr, updates_rcvr) = mpsc::channel(100);
            let pubsub_client =
                ChainPubsubClientMock::new(updates_sndr, updates_rcvr);
            (rpc_client, pubsub_client)
        };

        let test_engine = if let Some(capacity) = lru_capacity {
            let dirs = Dirs::default();
            let mut builder = keeper_builder(&dirs);
            builder.accountsdb.lru_capacity = capacity;
            Rc::new(
                TestEngine::from_builder(dirs, builder, Pacing::External).await,
            )
        } else {
            Rc::new(TestEngine::new().await)
        };
        let bank = Engine::clone(test_engine.as_ref());
        let validator_keypair = Keypair::new();
        let validator_pubkey = validator_keypair.pubkey();
        let fetch_cloner = {
            let (tx, rx) = tokio::sync::mpsc::channel(100);
            let subscribed_accounts =
                create_test_subscribed_accounts_with_config(&config);

            let provider = Arc::new(
                RemoteAccountProvider::try_from_clients_and_mode(
                    rpc_client.clone(),
                    pubsub_client.clone(),
                    tx,
                    &config,
                    subscribed_accounts,
                    Arc::<AtomicU64>::default(),
                )
                .await
                .expect("create remote account provider")
                .expect("ephemeral lifecycle enables remote accounts"),
            );
            FetchCloner::new(
                &provider,
                bank.clone(),
                validator_keypair.insecure_clone(),
                rx,
                None,
                risk_service,
            )
        };
        let chainlink =
            InnerChainlink::try_new(bank.clone(), Some(fetch_cloner)).unwrap();
        Self {
            rpc_client,
            pubsub_client,
            chainlink: Arc::new(chainlink),
            test_engine,
            bank,
            validator_pubkey,
        }
    }

    pub async fn send_account_update<T: Into<Account>>(
        &self,
        pubkey: Pubkey,
        account: T,
    ) {
        let account = account.into();
        // When a subscription update is sent this means that the Solana account updated and
        // thus it makes sense to keep our RpcClient in sync.
        self.rpc_client.add_account(pubkey, account.clone());
        let slot = self.rpc_client.get_slot();

        self.pubsub_client
            .send_account_update(pubkey, slot, &account)
            .await;
    }

    pub async fn wait_for_account_updates(
        &self,
        count: u64,
        timeout_millis: Option<u64>,
    ) -> bool {
        let timeout = timeout_millis
            .map(Duration::from_millis)
            .unwrap_or_else(|| Duration::from_secs(1));
        let fetch_cloner = self
            .chainlink
            .fetch_cloner()
            .expect("test Chainlink has a fetch cloner");
        let target_count = fetch_cloner.processed_updates_count() + count;
        self.wait_for_processed_account_updates(
            fetch_cloner,
            target_count,
            timeout,
        )
        .await
    }

    async fn wait_for_processed_account_updates(
        &self,
        fetch_cloner: &FetchCloner<ChainRpcClientMock, ChainPubsubClientMock>,
        target_count: u64,
        timeout: Duration,
    ) -> bool {
        trace!(
            "Waiting for {} account updates, current count: {}",
            target_count,
            fetch_cloner.processed_updates_count()
        );
        let start_time = Instant::now();
        while fetch_cloner.processed_updates_count() < target_count {
            tokio::task::yield_now().await;
            if start_time.elapsed() > timeout {
                return false;
            }
        }
        true
    }

    /// Sends an account update and waits for Chainlink to finish processing it.
    pub async fn send_and_receive_account_update<T: Into<Account>>(
        &self,
        pubkey: Pubkey,
        account: T,
        timeout_millis: Option<u64>,
    ) -> bool {
        let fetch_cloner = self
            .chainlink
            .fetch_cloner()
            .expect("test Chainlink has a fetch cloner");
        let target_count = fetch_cloner.processed_updates_count() + 1;
        self.send_account_update(pubkey, account).await;
        let timeout = timeout_millis
            .map(Duration::from_millis)
            .unwrap_or_else(|| Duration::from_secs(1));
        self.wait_for_processed_account_updates(
            fetch_cloner,
            target_count,
            timeout,
        )
        .await
    }

    pub async fn wait_for_local_account(
        bank: &Engine,
        pubkey: &Pubkey,
        updates: &mut mpsc::Receiver<AccountSharedData>,
        expected: &AccountSharedData,
    ) {
        let mut last = None;
        let result = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                // A borrowed account can expose its new image before AccountsDB
                // finishes moving it between storage backends. The account
                // notification is emitted only after that commit completes.
                updates
                    .recv()
                    .await
                    .expect("local account update channel closed");
                let matches = bank
                    .accounts()
                    .loader()
                    .read(pubkey, |account| account == expected)
                    .expect("load local account");
                if matches == Some(true) {
                    break;
                }
                last = bank
                    .accounts()
                    .loader()
                    .read(pubkey, |account| format!("{account:?}"))
                    .expect("load local account");
            }
        })
        .await;
        assert!(
            result.is_ok(),
            "timed out waiting for local account update: expected={expected:?}, last={last:?}"
        );
    }

    pub async fn ensure_account(&self, pubkey: &Pubkey) -> ChainlinkResult<()> {
        self.chainlink
            .ensure_accounts(
                &[*pubkey],
                AccountFetchContext::rpc_get_multiple_accounts(),
            )
            .await
            .map(|_| ())
    }

    /// Force undelegation of an account in the bank to mark it as such until
    /// the undelegation request on chain is processed
    pub async fn force_undelegation(&self, pubkey: &Pubkey) {
        // We modify the account direclty in the bank
        // normally this would happen as part of a transaction
        // Magicblock program marks account as undelegated in the Ephem
        let reader = |account: &AccountSharedData| {
            AccountBuilder::from(AccountSharedData::from(account.owned()))
                .owner(dlp_api::id())
                .mode(AccountMode::Transient)
        };
        let account = self
            .bank
            .accounts()
            .loader()
            .read(pubkey, reader)
            .expect("load local account")
            .expect("account exists before undelegation");
        self.bank
            .account(*pubkey)
            .await
            .materialize(account, None)
            .await
            .expect("mark account undelegating through engine");
    }

    /// Assumes that account was already marked as undelegate in the bank
    /// see [`force_undelegation`](Self::force_undelegation)
    #[allow(dead_code)]
    pub async fn commit_and_undelegate(
        &self,
        pubkey: &Pubkey,
        owner: &Pubkey,
    ) -> ChainlinkResult<AccountSharedData> {
        // Committor service calls this to trigger subscription
        self.chainlink.undelegation_requested(*pubkey).await?;

        // Committor service then requests undelegation on chain
        let acc = self.rpc_client.get_account_at_slot(pubkey).unwrap();
        let undelegated_acc: AccountSharedData =
            AccountBuilder::from(account_shared_with_owner_and_slot(
                &acc.account,
                *owner,
                self.rpc_client.get_slot(),
            ))
            .mode(AccountMode::ReadOnly)
            .build();
        let delegation_record_pubkey =
            dlp_api::pda::delegation_record_pda_from_delegated_account(pubkey);
        self.rpc_client.remove_account(&delegation_record_pubkey);
        let mut local_updates = self.bank.accounts().subscribe(*pubkey).await;
        self.send_account_update(
            *pubkey,
            AccountSharedData::from(undelegated_acc.owned()),
        )
        .await;
        Self::wait_for_local_account(
            &self.bank,
            pubkey,
            &mut local_updates,
            &undelegated_acc,
        )
        .await;

        Ok(undelegated_acc)
    }

    pub async fn delegate_existing_account_to(
        &self,
        pubkey: &Pubkey,
        authority: &Pubkey,
        owner: &Pubkey,
    ) -> ChainlinkResult<DelegateResult> {
        // Add new delegation record on chain
        let delegation_record_pubkey = add_delegation_record_for(
            &self.rpc_client,
            *pubkey,
            *authority,
            *owner,
        );

        // Update account to be delegated on chain and send a sub update
        let acc = self.rpc_client.get_account_at_slot(pubkey).unwrap();
        let delegated_acc =
            account_shared_with_owner(&acc.account, dlp_api::id());
        let mode = if authority == &self.validator_pubkey {
            AccountMode::Delegated
        } else {
            AccountMode::ReadOnly
        };
        let expected = AccountBuilder::from(AccountSharedData::from(
            delegated_acc.owned(),
        ))
        .owner(*owner)
        .slot(self.rpc_client.get_slot())
        .mode(mode)
        .build();
        let mut local_updates = self.bank.accounts().subscribe(*pubkey).await;
        self.send_account_update(*pubkey, delegated_acc).await;
        Self::wait_for_local_account(
            &self.bank,
            pubkey,
            &mut local_updates,
            &expected,
        )
        .await;

        Ok(DelegateResult {
            delegation_record_pubkey,
        })
    }
}

pub struct DelegateResult {
    pub delegation_record_pubkey: Pubkey,
}
