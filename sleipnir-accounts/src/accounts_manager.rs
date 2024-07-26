use std::sync::Arc;

use conjunto_transwise::{
    RpcProviderConfig, TransactionAccountsExtractorImpl, Transwise,
};
use sleipnir_bank::bank::Bank;
use sleipnir_transaction_status::TransactionStatusSender;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig, signature::Keypair, signer::Signer,
};

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

pub type AccountsManager = ExternalAccountsManager<
    BankAccountProvider,
    RemoteAccountCloner,
    RemoteAccountCommitter,
    Transwise,
    TransactionAccountsExtractorImpl,
    RemoteScheduledCommitsProcessor,
>;

impl
    ExternalAccountsManager<
        BankAccountProvider,
        RemoteAccountCloner,
        RemoteAccountCommitter,
        Transwise,
        TransactionAccountsExtractorImpl,
        RemoteScheduledCommitsProcessor,
    >
{
    pub fn try_new(
        bank: &Arc<Bank>,
        transaction_status_sender: Option<TransactionStatusSender>,
        validator_keypair: Keypair,
        config: AccountsConfig,
    ) -> AccountsResult<Self> {
        let validator_id = validator_keypair.pubkey();

        let external_config = config.external;
        let cluster = external_config.cluster;
        let internal_account_provider = BankAccountProvider::new(bank.clone());
        let rpc_cluster = try_rpc_cluster_from_cluster(&cluster)?;
        let rpc_client = RpcClient::new_with_commitment(
            rpc_cluster.url().to_string(),
            CommitmentConfig::confirmed(),
        );
        let rpc_provider_config = RpcProviderConfig::new(rpc_cluster, None);

        let account_cloner = RemoteAccountCloner::new(
            cluster.clone(),
            bank.clone(),
            transaction_status_sender.clone(),
        );
        let account_committer = RemoteAccountCommitter::new(
            rpc_client,
            validator_keypair,
            config.commit_compute_unit_price,
        );
        let validated_accounts_provider = Transwise::new(rpc_provider_config);

        let scheduled_commits_processor = RemoteScheduledCommitsProcessor::new(
            cluster,
            bank.clone(),
            transaction_status_sender,
        );

        Ok(Self {
            internal_account_provider,
            account_cloner,
            account_committer: Arc::new(account_committer),
            validated_accounts_provider,
            transaction_accounts_extractor: TransactionAccountsExtractorImpl,
            external_readonly_accounts: ExternalReadonlyAccounts::default(),
            external_writable_accounts: ExternalWritableAccounts::default(),
            external_readonly_mode: external_config.readonly,
            external_writable_mode: external_config.writable,
            create_accounts: config.create,
            scheduled_commits_processor,
            payer_init_lamports: config.payer_init_lamports,
            validator_id,
        })
    }
}
