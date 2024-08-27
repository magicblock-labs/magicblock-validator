use std::sync::Arc;

use crate::{
    bank_account_provider::BankAccountProvider,
    config::AccountsConfig,
    errors::AccountsResult,
    external_accounts::{ExternalReadonlyAccounts, ExternalWritableAccounts},
    remote_account_cloner::RemoteAccountCloner,
    remote_account_committer::RemoteAccountCommitter,
    remote_scheduled_commits_processor::RemoteScheduledCommitsProcessor,
    utils::try_rpc_cluster_from_cluster,
    ExternalAccountsManager,
};
use conjunto_transwise::{
    transaction_accounts_extractor::TransactionAccountsExtractorImpl,
    transaction_accounts_validator::TransactionAccountsValidatorImpl,
};
use sleipnir_account_fetcher::RemoteAccountFetcherClient;
use sleipnir_account_updates::RemoteAccountUpdatesClient;
use sleipnir_bank::bank::Bank;
use sleipnir_program::ValidatorAccountsRemover;
use sleipnir_transaction_status::TransactionStatusSender;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig, signature::Keypair, signer::Signer,
};

pub type AccountsManager = ExternalAccountsManager<
    BankAccountProvider,
    RemoteAccountFetcherClient,
    RemoteAccountCloner,
    RemoteAccountCommitter,
    ValidatorAccountsRemover,
    RemoteAccountUpdatesClient,
    TransactionAccountsExtractorImpl,
    TransactionAccountsValidatorImpl,
    RemoteScheduledCommitsProcessor,
>;

impl
ExternalAccountsManager<
    BankAccountProvider,
    RemoteAccountFetcherClient,
    RemoteAccountCloner,
    RemoteAccountCommitter,
    ValidatorAccountsRemover,
    RemoteAccountUpdatesClient,
    TransactionAccountsExtractorImpl,
    TransactionAccountsValidatorImpl,
    RemoteScheduledCommitsProcessor,
>
{
    pub fn try_new(
        bank: &Arc<Bank>,
        remote_account_fetcher_client: RemoteAccountFetcherClient,
        remote_account_updates_client: RemoteAccountUpdatesClient,
        transaction_status_sender: Option<TransactionStatusSender>,
        validator_keypair: Keypair,
        config: AccountsConfig,
    ) -> AccountsResult<Self> {
        let validator_id = validator_keypair.pubkey();

        let remote_cluster = config.remote_cluster;
        let internal_account_provider = BankAccountProvider::new(bank.clone());
        let rpc_cluster = try_rpc_cluster_from_cluster(&remote_cluster)?;
        let rpc_client = RpcClient::new_with_commitment(
            rpc_cluster.url().to_string(),
            CommitmentConfig::confirmed(),
        );
        let account_cloner = RemoteAccountCloner::new(
            remote_cluster.clone(),
            bank.clone(),
            transaction_status_sender.clone(),
        );
        let account_committer = RemoteAccountCommitter::new(
            rpc_client,
            validator_keypair,
            config.commit_compute_unit_price,
        );

        let scheduled_commits_processor = RemoteScheduledCommitsProcessor::new(
            remote_cluster,
            bank.clone(),
            transaction_status_sender,
        );

        Ok(Self {
            internal_account_provider,
            account_fetcher: remote_account_fetcher_client,
            account_cloner,
            account_committer: Arc::new(account_committer),
            accounts_remover: ValidatorAccountsRemover::default(),
            account_updates: remote_account_updates_client,
            transaction_accounts_extractor: TransactionAccountsExtractorImpl,
            transaction_accounts_validator: TransactionAccountsValidatorImpl,
            external_readonly_accounts: ExternalReadonlyAccounts::default(),
            external_writable_accounts: ExternalWritableAccounts::default(),
            lifecycle: config.lifecycle,
            scheduled_commits_processor,
            payer_init_lamports: config.payer_init_lamports,
            validator_id,
        })
    }
}
