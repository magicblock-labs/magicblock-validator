use conjunto_transwise::{
    trans_account_meta::TransactionAccountsHolder,
    validated_accounts::ValidateAccountsConfig, TransactionAccountsExtractor,
    ValidatedAccountsProvider,
};
use solana_sdk::{signature::Signature, transaction::SanitizedTransaction};

use crate::{
    errors::AccountsResult,
    external_accounts::{ExternalReadonlyAccounts, ExternalWritableAccounts},
    traits::{AccountCloner, InternalAccountProvider},
};
pub struct ExternalAccountsManager<IAP, AC, VAP>
where
    IAP: InternalAccountProvider,
    AC: AccountCloner,
    VAP: ValidatedAccountsProvider + TransactionAccountsExtractor,
{
    internal_account_provider: IAP,
    account_cloner: AC,
    validated_accounts_provider: VAP,
    validate_config: ValidateAccountsConfig,
    external_readonly_accounts: ExternalReadonlyAccounts,
    external_writable_accounts: ExternalWritableAccounts,
}

impl<IAP, AC, VAP> ExternalAccountsManager<IAP, AC, VAP>
where
    IAP: InternalAccountProvider,
    AC: AccountCloner,
    VAP: ValidatedAccountsProvider + TransactionAccountsExtractor,
{
    pub async fn ensure_accounts(
        &self,
        tx: &SanitizedTransaction,
    ) -> AccountsResult<Vec<Signature>> {
        // 1. Extract all acounts from the transaction
        let accounts_holder = self
            .validated_accounts_provider
            .accounts_from_sanitized_transaction(tx);

        // 2. Remove all accounts we already track as external accounts
        //    and the ones that are found in our validator
        let new_readonly_accounts = accounts_holder
            .readonly
            .into_iter()
            // 1. Filter external readonly accounts we already know about and cloned
            //    They would also be found via the internal account provider, but this
            //    is a faster lookup
            .filter(|pubkey| !self.external_readonly_accounts.has(pubkey))
            // 2. Filter accounts that are found inside our validator (slower looukup)
            .filter(|pubkey| {
                self.internal_account_provider.get_account(pubkey).is_none()
            });

        let new_writable_accounts = accounts_holder
            .writable
            .into_iter()
            .filter(|pubkey| !self.external_writable_accounts.has(pubkey))
            .filter(|pubkey| {
                self.internal_account_provider.get_account(pubkey).is_none()
            });

        // 3. Validate only the accounts that we see for the very first time
        let validated_accounts = self
            .validated_accounts_provider
            .validate_accounts(
                &TransactionAccountsHolder {
                    readonly: new_readonly_accounts.collect(),
                    writable: new_writable_accounts.collect(),
                },
                &self.validate_config,
            )
            .await?;

        // 4. Clone the accounts and add metadata to external account trackers
        let mut signatures = vec![];
        for readonly in validated_accounts.readonly {
            let signature =
                self.account_cloner.clone_account(&readonly).await?;
            signatures.push(signature);
            self.external_readonly_accounts.insert(readonly);
        }

        for writable in validated_accounts.writable {
            let signature =
                self.account_cloner.clone_account(&writable).await?;
            signatures.push(signature);
            self.external_writable_accounts.insert(writable);
        }

        Ok(signatures)
    }
}
