#![allow(unused)]
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use log::*;
use magicblock_chainlink::{
    accounts_bank::mock::AccountsBankStub,
    config::LifecycleMode,
    errors::ChainlinkResult,
    fetch_cloner::{FetchAndCloneResult, FetchCloner},
    remote_account_provider::{
        chain_pubsub_client::{mock::ChainPubsubClientMock, ChainPubsubClient},
        config::RemoteAccountProviderConfig,
        RemoteAccountProvider,
    },
    testing::{
        accounts::account_shared_with_owner,
        cloner_stub::ClonerStub,
        deleg::add_delegation_record_for,
        rpc_client_mock::{ChainRpcClientMock, ChainRpcClientMockBuilder},
    },
    Chainlink,
};
use solana_account::{Account, AccountSharedData};
use solana_pubkey::Pubkey;
use solana_sdk::{clock::Slot, sysvar::clock};
use tokio::sync::mpsc;

use super::accounts::account_shared_with_owner_and_slot;
pub type TestChainlink = Chainlink<
    ChainRpcClientMock,
    ChainPubsubClientMock,
    AccountsBankStub,
    ClonerStub,
>;

#[derive(Clone)]
pub struct TestContext {
    pub rpc_client: ChainRpcClientMock,
    pub pubsub_client: ChainPubsubClientMock,
    pub chainlink: Arc<TestChainlink>,
    pub bank: Arc<AccountsBankStub>,
    pub remote_account_provider: Option<
        Arc<RemoteAccountProvider<ChainRpcClientMock, ChainPubsubClientMock>>,
    >,
    pub cloner: Arc<ClonerStub>,
    pub validator_pubkey: Pubkey,
}

impl TestContext {
    pub async fn init(slot: Slot) -> Self {
        let (rpc_client, pubsub_client) = {
            let rpc_client =
                ChainRpcClientMockBuilder::new().slot(slot).build();
            let (updates_sndr, updates_rcvr) = mpsc::channel(100);
            let pubsub_client =
                ChainPubsubClientMock::new(updates_sndr, updates_rcvr);
            (rpc_client, pubsub_client)
        };

        let lifecycle_mode = LifecycleMode::Ephemeral;
        let bank = Arc::<AccountsBankStub>::default();
        let cloner = Arc::new(ClonerStub::new(bank.clone()));
        let validator_pubkey = Pubkey::new_unique();
        let faucet_pubkey = Pubkey::new_unique();
        let (fetch_cloner, remote_account_provider) = {
            let (tx, rx) = tokio::sync::mpsc::channel(100);
            let config = RemoteAccountProviderConfig::try_new_with_metrics(
                1000, // subscribed_accounts_lru_capacity
                lifecycle_mode,
                false, // disable subscription metrics
            )
            .unwrap();
            let remote_account_provider =
                RemoteAccountProvider::try_from_clients_and_mode(
                    rpc_client.clone(),
                    pubsub_client.clone(),
                    tx,
                    &config,
                )
                .await;

            match remote_account_provider {
                Ok(Some(remote_account_provider)) => {
                    debug!("Initializing FetchCloner");
                    let provider = Arc::new(remote_account_provider);
                    (
                        Some(FetchCloner::new(
                            &provider,
                            &bank,
                            &cloner,
                            validator_pubkey,
                            faucet_pubkey,
                            rx,
                        )),
                        Some(provider),
                    )
                }
                Err(err) => {
                    panic!("Failed to create remote account provider: {err:?}");
                }
                _ => (None, None),
            }
        };
        let chainlink = Chainlink::try_new(
            &bank,
            fetch_cloner,
            validator_pubkey,
            faucet_pubkey,
            0,
        )
        .unwrap();
        Self {
            rpc_client,
            pubsub_client,
            chainlink: Arc::new(chainlink),
            bank,
            cloner,
            validator_pubkey,
            remote_account_provider,
        }
    }

    #[allow(dead_code)]
    pub async fn wait_for_account_updates(
        &self,
        count: u64,
        timeout_millis: Option<u64>,
    ) -> bool {
        let timeout = timeout_millis
            .map(Duration::from_millis)
            .unwrap_or_else(|| Duration::from_secs(1));
        if let Some(fetch_cloner) = self.chainlink.fetch_cloner() {
            let target_count = fetch_cloner.received_updates_count() + count;
            trace!(
                "Waiting for {} account updates, current count: {}",
                target_count,
                fetch_cloner.received_updates_count()
            );
            let start_time = Instant::now();
            while fetch_cloner.received_updates_count() < target_count {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                if start_time.elapsed() > timeout {
                    return false;
                }
            }
            true
        } else {
            true
        }
    }

    #[allow(dead_code)]
    pub async fn send_account_update(&self, pubkey: Pubkey, account: &Account) {
        // When a subscription update is sent this means that the Solana account updated and
        // thus it makes sense to keep our RpcClient in sync.
        self.rpc_client.add_account(pubkey, account.clone());
        let slot = self.rpc_client.get_slot();

        self.pubsub_client
            .send_account_update(pubkey, slot, account)
            .await;
    }

    /// Sends an account update via the pubsub client and
    /// waits for the remote account provider to receive it.
    #[allow(dead_code)]
    pub async fn send_and_receive_account_update<T: Into<Account>>(
        &self,
        pubkey: Pubkey,
        account: T,
        timeout_millis: Option<u64>,
    ) -> bool {
        self.send_account_update(pubkey, &account.into()).await;
        self.wait_for_account_updates(1, timeout_millis).await
    }

    #[allow(dead_code)]
    pub async fn send_removal_update(&self, pubkey: Pubkey) {
        let acc = Account::default();
        self.send_account_update(pubkey, &acc).await;
    }

    #[allow(dead_code)]
    pub async fn update_slot(&self, slot: Slot) {
        self.rpc_client.set_current_slot(slot);
        assert!(
            self.send_and_receive_account_update(
                clock::ID,
                Account::default(),
                Some(1000),
            )
            .await,
            "Failed to update clock sysvar after 1 sec"
        );
    }

    #[allow(dead_code)]
    pub async fn ensure_account(
        &self,
        pubkey: &Pubkey,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        self.chainlink
            .ensure_accounts(
                &[*pubkey],
                None,
                magicblock_chainlink::AccountFetchOrigin::GetAccount,
            )
            .await
    }

    /// Force undelegation of an account in the bank to mark it as such until
    /// the undelegation request on chain is processed
    #[allow(dead_code)]
    pub fn force_undelegation(&self, pubkey: &Pubkey) {
        // We modify the account direclty in the bank
        // normally this would happen as part of a transaction
        // Magicblock program marks account as undelegated in the Ephem
        self.bank.force_undelegation(pubkey)
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
        let undelegated_acc = account_shared_with_owner_and_slot(
            &acc.account,
            *owner,
            self.rpc_client.get_slot(),
        );
        let delegation_record_pubkey =
            dlp::pda::delegation_record_pda_from_delegated_account(pubkey);
        self.rpc_client.remove_account(&delegation_record_pubkey);
        let updated = self
            .send_and_receive_account_update(
                *pubkey,
                undelegated_acc.clone(),
                Some(400),
            )
            .await;
        assert!(updated, "Failed to receive undelegation update");

        Ok(undelegated_acc)
    }

    #[allow(dead_code)]
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
        let delegated_acc = account_shared_with_owner(&acc.account, dlp::id());
        let updated = self
            .send_and_receive_account_update(
                *pubkey,
                delegated_acc.clone(),
                Some(400),
            )
            .await;
        assert!(updated, "Failed to receive delegation update");

        Ok(DelegateResult {
            delegated_account: delegated_acc,
            delegation_record_pubkey,
        })
    }
}

#[allow(dead_code)]
pub struct DelegateResult {
    pub delegated_account: AccountSharedData,
    pub delegation_record_pubkey: Pubkey,
}
