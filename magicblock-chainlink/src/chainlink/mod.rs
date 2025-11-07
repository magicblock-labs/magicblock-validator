use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use dlp::pda::ephemeral_balance_pda_from_payer;
use errors::ChainlinkResult;
use fetch_cloner::FetchCloner;
use log::*;
use magicblock_core::traits::AccountsBank;
use solana_account::{AccountSharedData, ReadableAccount};
use solana_pubkey::Pubkey;
use solana_sdk::{
    commitment_config::CommitmentConfig, transaction::SanitizedTransaction,
};
use tokio::{sync::mpsc, task};

use crate::{
    cloner::Cloner,
    config::ChainlinkConfig,
    fetch_cloner::FetchAndCloneResult,
    remote_account_provider::{
        ChainPubsubClient, ChainPubsubClientImpl, ChainRpcClient,
        ChainRpcClientImpl, Endpoint, RemoteAccountProvider,
    },
    submux::SubMuxClient,
};

mod blacklisted_accounts;
pub mod config;
pub mod errors;
pub mod fetch_cloner;

pub use blacklisted_accounts::*;

// -----------------
// Chainlink
// -----------------
pub struct Chainlink<
    T: ChainRpcClient,
    U: ChainPubsubClient,
    V: AccountsBank,
    C: Cloner,
> {
    accounts_bank: Arc<V>,
    fetch_cloner: Option<Arc<FetchCloner<T, U, V, C>>>,
    /// The subscription to events for each account that is removed from
    /// the accounts tracked by the provider.
    /// In that case we also remove it from the bank since it is no longer
    /// synchronized.
    #[allow(unused)] // needed to cleanup chainlink
    removed_accounts_sub: Option<task::JoinHandle<()>>,

    validator_id: Pubkey,
    faucet_id: Pubkey,
}

impl<T: ChainRpcClient, U: ChainPubsubClient, V: AccountsBank, C: Cloner>
    Chainlink<T, U, V, C>
{
    pub fn try_new(
        accounts_bank: &Arc<V>,
        fetch_cloner: Option<Arc<FetchCloner<T, U, V, C>>>,
        validator_pubkey: Pubkey,
        faucet_pubkey: Pubkey,
    ) -> ChainlinkResult<Self> {
        let removed_accounts_sub = if let Some(fetch_cloner) = &fetch_cloner {
            let removed_accounts_rx =
                fetch_cloner.try_get_removed_account_rx()?;
            Some(Self::subscribe_account_removals(
                accounts_bank,
                removed_accounts_rx,
            ))
        } else {
            None
        };
        Ok(Self {
            accounts_bank: accounts_bank.clone(),
            fetch_cloner,
            removed_accounts_sub,
            validator_id: validator_pubkey,
            faucet_id: faucet_pubkey,
        })
    }

    pub async fn try_new_from_endpoints(
        endpoints: &[Endpoint],
        commitment: CommitmentConfig,
        accounts_bank: &Arc<V>,
        cloner: &Arc<C>,
        validator_pubkey: Pubkey,
        faucet_pubkey: Pubkey,
        config: ChainlinkConfig,
    ) -> ChainlinkResult<
        Chainlink<
            ChainRpcClientImpl,
            SubMuxClient<ChainPubsubClientImpl>,
            V,
            C,
        >,
    > {
        // Extract accounts provider and create fetch cloner while connecting
        // the subscription channel
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        let account_provider = RemoteAccountProvider::try_from_urls_and_config(
            endpoints,
            commitment,
            tx,
            &config.remote_account_provider,
        )
        .await?;
        let fetch_cloner = if let Some(provider) = account_provider {
            let provider = Arc::new(provider);
            let fetch_cloner = FetchCloner::new(
                &provider,
                accounts_bank,
                cloner,
                validator_pubkey,
                faucet_pubkey,
                rx,
            );
            Some(fetch_cloner)
        } else {
            None
        };

        Chainlink::try_new(
            accounts_bank,
            fetch_cloner,
            validator_pubkey,
            faucet_pubkey,
        )
    }

    /// Removes all accounts that aren't delegated to us and not blacklisted from the bank
    /// This should only be called _before_ the validator starts up, i.e.
    /// when resuming an existing ledger to guarantee that we don't hold
    /// accounts that might be stale.
    pub fn reset_accounts_bank(&self) {
        let blacklisted_accounts =
            blacklisted_accounts(&self.validator_id, &self.faucet_id);

        let delegated = AtomicU64::new(0);
        let dlp_owned_not_delegated = AtomicU64::new(0);
        let blacklisted = AtomicU64::new(0);
        let remaining = AtomicU64::new(0);

        let removed = self.accounts_bank.remove_where(|pubkey, account| {
            if blacklisted_accounts.contains(pubkey) {
                blacklisted.fetch_add(1, Ordering::Relaxed);
                return false;
            }
            if account.delegated() {
                delegated.fetch_add(1, Ordering::Relaxed);
                return false;
            }
            if account.owner().eq(&dlp::id()) {
                dlp_owned_not_delegated.fetch_add(1, Ordering::Relaxed);
                return true;
            }
            // Non-delegated, nor DLP-owned, nor blacklisted
            remaining.fetch_add(1, Ordering::Relaxed);
            true
        });

        info!(
            "Removed {removed} accounts from bank:
{} DLP-owned non-delegated
{} other non-delegated non-blacklisted.
Kept: {} delegated, {} blacklisted",
            dlp_owned_not_delegated.into_inner(),
            remaining.into_inner(),
            delegated.into_inner(),
            blacklisted.into_inner()
        );
    }

    fn subscribe_account_removals(
        accounts_bank: &Arc<V>,
        mut removed_accounts_rx: mpsc::Receiver<Pubkey>,
    ) -> task::JoinHandle<()> {
        let accounts_bank = accounts_bank.clone();

        task::spawn(async move {
            while let Some(pubkey) = removed_accounts_rx.recv().await {
                accounts_bank.remove_account(&pubkey);
            }
            warn!("Removed accounts channel closed, stopping subscription");
        })
    }

    /// This method ensures that the accounts rise to the top of used accounts, no
    /// matter if we end up cloning/subscribing to them or not.
    /// For new accounts this would not be needed as they are promoted when
    /// they are added, but for existing accounts that step is never taken.
    /// For those accounts that weren't subscribed to yet (new accounts) this
    /// does nothing as only existing accounts are affected.
    /// See [lru::LruCache::promote]
    fn promote_accounts(
        fetch_cloner: &FetchCloner<T, U, V, C>,
        pubkeys: &[&Pubkey],
    ) {
        fetch_cloner.promote_accounts(pubkeys);
    }

    /// Ensures that all accounts required by the transaction exist on chain,
    /// are delegated to our validator if writable and that their latest state
    /// is cloned in our validator.
    /// Returns the state of each account (writable and readonly) after the checks
    /// and cloning are done.
    pub async fn ensure_transaction_accounts(
        &self,
        tx: &SanitizedTransaction,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        let mut pubkeys = tx
            .message()
            .account_keys()
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let feepayer = tx.message().fee_payer();
        // In the case of transactions we need to clone the feepayer account
        let clone_escrow = {
            // If the fee payer account is in the bank we only clone the balance
            // escrow account if the fee payer is not delegated
            // If it is not in the bank we include it just in case, it is fine
            // if it doesn't exist and once we cloned the feepayer account itself
            // and it turns out to be delegated, then we will avoid cloning the
            // escrow account next time
            self.accounts_bank
                .get_account(feepayer)
                .is_none_or(|a| !a.delegated())
        };

        let mark_empty_if_not_found = if clone_escrow {
            let balance_pda = ephemeral_balance_pda_from_payer(feepayer, 0);
            trace!("Adding balance PDA {balance_pda} for feepayer {feepayer}");
            pubkeys.push(balance_pda);
            vec![balance_pda]
        } else {
            vec![]
        };
        let mark_empty_if_not_found = (!mark_empty_if_not_found.is_empty())
            .then(|| &mark_empty_if_not_found[..]);
        self.ensure_accounts(&pubkeys, mark_empty_if_not_found)
            .await
    }

    /// Same as fetch accounts, but does not return the accounts, just
    /// ensures were cloned into our validator if they exist on chain.
    /// If we're offline and not syncing accounts then this is a no-op.
    pub async fn ensure_accounts(
        &self,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        let Some(fetch_cloner) = self.fetch_cloner() else {
            return Ok(FetchAndCloneResult::default());
        };
        self.fetch_accounts_common(
            fetch_cloner,
            pubkeys,
            mark_empty_if_not_found,
        )
        .await
    }

    /// Fetches the accounts from the bank if we're offline and not syncing accounts.
    /// Otherwise ensures that the accounts exist on chain and were cloned into our validator
    /// and returns their state from the bank (which may be None if the account does not
    /// exist locally or on chain).
    pub async fn fetch_accounts(
        &self,
        pubkeys: &[Pubkey],
    ) -> ChainlinkResult<Vec<Option<AccountSharedData>>> {
        if log::log_enabled!(log::Level::Trace) {
            let pubkeys = pubkeys
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            trace!("Fetching accounts: {pubkeys}");
        }
        let Some(fetch_cloner) = self.fetch_cloner() else {
            // If we're offline and not syncing accounts then we just get them from the bank
            return Ok(pubkeys
                .iter()
                .map(|pubkey| self.accounts_bank.get_account(pubkey))
                .collect());
        };
        let _ = self
            .fetch_accounts_common(fetch_cloner, pubkeys, None)
            .await?;

        let accounts = pubkeys
            .iter()
            .map(|pubkey| self.accounts_bank.get_account(pubkey))
            .collect();
        Ok(accounts)
    }

    async fn fetch_accounts_common(
        &self,
        fetch_cloner: &FetchCloner<T, U, V, C>,
        pubkeys: &[Pubkey],
        mark_empty_if_not_found: Option<&[Pubkey]>,
    ) -> ChainlinkResult<FetchAndCloneResult> {
        if log::log_enabled!(log::Level::Trace) {
            let pubkeys_str = pubkeys
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let mark_empty_str = mark_empty_if_not_found
                .map(|keys| {
                    keys.iter()
                        .map(|p| p.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            trace!("Fetching accounts: {pubkeys_str}, mark_empty_if_not_found: {mark_empty_str}");
        }
        Self::promote_accounts(
            fetch_cloner,
            &pubkeys.iter().collect::<Vec<_>>(),
        );

        // If any of the accounts was invalid and couldn't be fetched/cloned then
        // we return an error.
        let result = fetch_cloner
            .fetch_and_clone_accounts_with_dedup(
                pubkeys,
                mark_empty_if_not_found,
                None,
            )
            .await?;
        trace!("Fetched and cloned accounts: {result:?}");
        Ok(result)
    }

    /// This is called via the committor service when an account is about to be undelegated
    /// At this point we do the following:
    /// 1. Subscribe to updates for the account
    /// 2. When a subscription update is received we clone the new state as usual
    pub async fn undelegation_requested(
        &self,
        pubkey: Pubkey,
    ) -> ChainlinkResult<()> {
        trace!("Undelegation requested for account: {pubkey}");

        let Some(fetch_cloner) = self.fetch_cloner() else {
            return Ok(());
        };

        // Subscribe to updates for this account so we can track changes
        // once it's undelegated
        fetch_cloner.subscribe_to_account(&pubkey).await?;

        trace!("Successfully subscribed to account {pubkey} for undelegation tracking");
        Ok(())
    }

    pub fn fetch_cloner(&self) -> Option<&Arc<FetchCloner<T, U, V, C>>> {
        self.fetch_cloner.as_ref()
    }

    pub fn fetch_count(&self) -> Option<u64> {
        self.fetch_cloner().map(|provider| provider.fetch_count())
    }

    pub fn is_watching(&self, pubkey: &Pubkey) -> bool {
        self.fetch_cloner()
            .map(|provider| provider.is_watching(pubkey))
            .unwrap_or(false)
    }
}
