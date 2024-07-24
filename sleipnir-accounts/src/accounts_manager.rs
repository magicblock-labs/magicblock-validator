use std::sync::Arc;

use conjunto_transwise::{
    RpcProviderConfig, TransactionAccountsExtractorImpl, Transwise,
};
use sleipnir_bank::bank::Bank;
use sleipnir_transaction_status::TransactionStatusSender;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{commitment_config::CommitmentConfig, signature::Keypair};

use crate::{
    bank_account_provider::BankAccountProvider,
    config::AccountsConfig,
    errors::AccountsResult,
    external_accounts::{ExternalReadonlyAccounts, ExternalWritableAccounts},
    remote_account_cloner::RemoteAccountCloner,
    remote_account_committer::RemoteAccountCommitter,
    utils::try_rpc_cluster_from_cluster,
    ExternalAccountsManager,
};

pub type AccountsManager = ExternalAccountsManager<
    BankAccountProvider,
    RemoteAccountCloner,
    RemoteAccountCommitter,
    Transwise,
    TransactionAccountsExtractorImpl,
>;

impl
    ExternalAccountsManager<
        BankAccountProvider,
        RemoteAccountCloner,
        RemoteAccountCommitter,
        Transwise,
        TransactionAccountsExtractorImpl,
    >
{
    pub fn try_new(
        bank: &Arc<Bank>,
        transaction_status_sender: Option<TransactionStatusSender>,
        committer_authority: Keypair,
        config: AccountsConfig,
    ) -> AccountsResult<Self> {
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
            cluster,
            bank.clone(),
            transaction_status_sender,
        );
        let account_committer = RemoteAccountCommitter::new(
            rpc_client,
            committer_authority,
            config.commit_compute_unit_price,
        );
        let validated_accounts_provider = Transwise::new(rpc_provider_config);

        Ok(Self {
            internal_account_provider,
            account_cloner,
            account_committer,
            validated_accounts_provider,
            transaction_accounts_extractor: TransactionAccountsExtractorImpl,
            external_readonly_accounts: ExternalReadonlyAccounts::default(),
            external_writable_accounts: ExternalWritableAccounts::default(),
            external_readonly_mode: external_config.readonly,
            external_writable_mode: external_config.writable,
            create_accounts: config.create,
            payer_init_lamports: config.payer_init_lamports,
        })
    }
}
