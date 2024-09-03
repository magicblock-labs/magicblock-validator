pub mod account_committer_stub;
pub mod scheduled_commits_processor_stub;

pub(crate) type StubbedAccountsManager = ExternalAccountsManager<
    internal_account_provider_stub::InternalAccountProviderStub,
    account_fetcher_stub::AccountFetcherStub,
    account_cloner_stub::AccountClonerStub,
    account_committer_stub::AccountCommitterStub,
    accounts_remover_stub::AccountsRemoverStub,
    account_updates_stub::AccountUpdatesStub,
    conjunto_transwise::transaction_accounts_extractor::TransactionAccountsExtractorImpl,
    conjunto_transwise::transaction_accounts_validator::TransactionAccountsValidatorImpl,
    scheduled_commits_processor_stub::ScheduledCommitsProcessorStub,
>;
