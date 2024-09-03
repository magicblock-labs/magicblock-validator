use std::sync::Arc;

use conjunto_transwise::{
    transaction_accounts_extractor::TransactionAccountsExtractorImpl,
    transaction_accounts_validator::TransactionAccountsValidatorImpl,
};
use sleipnir_account_cloner::RemoteAccountClonerClient;
use sleipnir_accounts_api::BankAccountProvider;
use sleipnir_bank::bank::Bank;
use sleipnir_program::ValidatorAccountsRemover;
use sleipnir_transaction_status::TransactionStatusSender;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{commitment_config::CommitmentConfig, signature::Keypair};

use crate::{
    config::AccountsConfig, errors::AccountsResult,
    remote_account_committer::RemoteAccountCommitter,
    remote_scheduled_commits_processor::RemoteScheduledCommitsProcessor,
    utils::try_rpc_cluster_from_cluster, ExternalAccountsManager,
};

pub type AccountsManager = ExternalAccountsManager<
    BankAccountProvider,
    RemoteAccountClonerClient,
    RemoteAccountCommitter,
    ValidatorAccountsRemover,
    TransactionAccountsExtractorImpl,
    TransactionAccountsValidatorImpl,
    RemoteScheduledCommitsProcessor,
>;

impl AccountsManager {
    pub fn try_new(
        bank: &Arc<Bank>,
        remote_account_cloner_client: RemoteAccountClonerClient,
        transaction_status_sender: Option<TransactionStatusSender>,
        validator_keypair: Keypair,
        config: AccountsConfig,
    ) -> AccountsResult<Self> {
        let remote_cluster = config.remote_cluster;
        let internal_account_provider = BankAccountProvider::new(bank.clone());
        let rpc_cluster = try_rpc_cluster_from_cluster(&remote_cluster)?;
        let rpc_client = RpcClient::new_with_commitment(
            rpc_cluster.url().to_string(),
            CommitmentConfig::confirmed(),
        );
        let account_committer = RemoteAccountCommitter::new(
            rpc_client,
            validator_keypair,
            config.commit_compute_unit_price,
        );

        let scheduled_commits_processor = RemoteScheduledCommitsProcessor::new(
            remote_cluster,
            bank.clone(),
            transaction_status_sender.clone(),
        );

        Ok(Self {
            internal_account_provider,
            account_cloner: remote_account_cloner_client,
            account_committer: Arc::new(account_committer),
            accounts_remover: ValidatorAccountsRemover::default(),
            transaction_accounts_extractor: TransactionAccountsExtractorImpl,
            transaction_accounts_validator: TransactionAccountsValidatorImpl,
            lifecycle: config.lifecycle,
            scheduled_commits_processor,
            external_commitable_accounts: Default::default(),
        })
    }
}
