use std::sync::Arc;

use conjunto_transwise::{
    transaction_accounts_extractor::TransactionAccountsExtractorImpl,
    transaction_accounts_validator::TransactionAccountsValidatorImpl,
};
use magicblock_account_cloner::RemoteAccountClonerClient;
use magicblock_accounts_api::BankAccountProvider;
use magicblock_bank::bank::Bank;
use magicblock_committor_service::{
    service_ext::CommittorServiceExt, CommittorService,
};

use crate::{
    config::AccountsConfig, errors::AccountsResult, ExternalAccountsManager,
};

pub type AccountsManager = ExternalAccountsManager<
    BankAccountProvider,
    RemoteAccountClonerClient,
    TransactionAccountsExtractorImpl,
    TransactionAccountsValidatorImpl,
    CommittorServiceExt<CommittorService>,
>;

impl AccountsManager {
    pub fn try_new(
        bank: &Arc<Bank>,
        committor_service: Arc<CommittorServiceExt<CommittorService>>,
        remote_account_cloner_client: RemoteAccountClonerClient,
        config: AccountsConfig,
    ) -> AccountsResult<Self> {
        let internal_account_provider = BankAccountProvider::new(bank.clone());

        Ok(Self {
            committor_service,
            internal_account_provider,
            account_cloner: remote_account_cloner_client,
            transaction_accounts_extractor: TransactionAccountsExtractorImpl,
            transaction_accounts_validator: TransactionAccountsValidatorImpl,
            lifecycle: config.lifecycle,
            external_commitable_accounts: Default::default(),
        })
    }
}
