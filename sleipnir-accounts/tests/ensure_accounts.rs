use conjunto_transwise::{
    trans_account_meta::TransactionAccountsHolder,
    validated_accounts::ValidateAccountsConfig,
};
use sleipnir_accounts::ExternalAccountsManager;
use solana_sdk::pubkey::Pubkey;
use utils::stubs::{
    AccountClonerStub, InternalAccountProviderStub,
    ValidatedAccountsProviderStub,
};

mod utils;

fn setup(
    internal_account_provider: InternalAccountProviderStub,
    account_cloner: AccountClonerStub,
    validated_accounts_provider: ValidatedAccountsProviderStub,
) -> ExternalAccountsManager<
    InternalAccountProviderStub,
    AccountClonerStub,
    ValidatedAccountsProviderStub,
> {
    ExternalAccountsManager {
        internal_account_provider,
        account_cloner,
        validated_accounts_provider,
        validate_config: ValidateAccountsConfig::default(),
        external_readonly_accounts: Default::default(),
        external_writable_accounts: Default::default(),
    }
}

#[tokio::test]
async fn test_ensure_readonly_accounts_none_tracked_nor_in_our_validator() {
    let internal_account_provider = InternalAccountProviderStub::default();

    let account_cloner = AccountClonerStub::default();
    let validated_accounts_provider = ValidatedAccountsProviderStub::valid();

    let manager = setup(
        internal_account_provider,
        account_cloner,
        validated_accounts_provider,
    );

    let readonly = Pubkey::new_unique();
    let holder = TransactionAccountsHolder {
        readonly: vec![readonly],
        writable: vec![],
    };

    let result = manager.ensure_accounts_from_holder(holder).await;
    eprintln!("{:?}", result);
}
