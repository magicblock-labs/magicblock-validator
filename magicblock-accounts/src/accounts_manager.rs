use std::sync::Arc;

use conjunto_transwise::{
    transaction_accounts_extractor::TransactionAccountsExtractorImpl,
    transaction_accounts_validator::TransactionAccountsValidatorImpl,
};
use magicblock_account_cloner::RemoteAccountClonerClient;
use magicblock_accounts_api::AccountsDbProvider;
use magicblock_accounts_db::AccountsDb;
use magicblock_committor_service::{
    service_ext::CommittorServiceExt, CommittorService,
};
use magicblock_core::link::transactions::TransactionSchedulerHandle;
use magicblock_ledger::LatestBlock;

use crate::{
    config::AccountsConfig, errors::AccountsResult, ExternalAccountsManager,
};

pub type AccountsManager = ExternalAccountsManager<
    AccountsDbProvider,
    RemoteAccountClonerClient,
    TransactionAccountsExtractorImpl,
    TransactionAccountsValidatorImpl,
    CommittorServiceExt<CommittorService>,
>;

impl AccountsManager {
    pub fn try_new(
        bank: &Arc<AccountsDb>,
        committor_service: Option<Arc<CommittorServiceExt<CommittorService>>>,
        remote_account_cloner_client: RemoteAccountClonerClient,
        config: AccountsConfig,
        internal_transaction_scheduler: TransactionSchedulerHandle,
        latest_block: LatestBlock,
    ) -> AccountsResult<Self> {
        let internal_account_provider = AccountsDbProvider::new(bank.clone());

        Ok(Self {
            committor_service,
            internal_account_provider,
            account_cloner: remote_account_cloner_client,
            transaction_accounts_extractor: TransactionAccountsExtractorImpl,
            transaction_accounts_validator: TransactionAccountsValidatorImpl,
            lifecycle: config.lifecycle,
            external_commitable_accounts: Default::default(),
            internal_transaction_scheduler,
            latest_block,
        })
    }
}
