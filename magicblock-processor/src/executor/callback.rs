use magicblock_accounts_db::traits::AccountsBank;
use solana_account::{AccountSharedData, WritableAccount};
use solana_feature_set::FeatureSet;
use solana_fee::FeeFeatures;
use solana_fee_structure::FeeDetails;
use solana_pubkey::Pubkey;
use solana_sdk_ids::native_loader;
use solana_svm::transaction_processing_callback::TransactionProcessingCallback;
use solana_svm_transaction::svm_message::SVMMessage;

use super::TransactionExecutor;

/// Required implementation to use the executor within the SVM.
impl TransactionProcessingCallback for TransactionExecutor {
    fn account_matches_owners(
        &self,
        account: &Pubkey,
        owners: &[Pubkey],
    ) -> Option<usize> {
        self.accountsdb.account_matches_owners(account, owners)
    }

    fn get_account_shared_data(
        &self,
        pubkey: &Pubkey,
    ) -> Option<AccountSharedData> {
        self.accountsdb.get_account(pubkey)
    }

    fn add_builtin_account(&self, name: &str, program_id: &Pubkey) {
        if self.accountsdb.contains_account(program_id) {
            return;
        }

        // Create a placeholder executable account for the builtin.
        let mut account =
            AccountSharedData::new(1, name.len(), &native_loader::ID);
        account.set_data_from_slice(name.as_bytes());
        account.set_executable(true);

        let _ = self.accountsdb.insert_account(program_id, &account);
    }

    fn calculate_fee(
        &self,
        message: &impl SVMMessage,
        lamports_per_signature: u64,
        prioritization_fee: u64,
        feature_set: &FeatureSet,
    ) -> FeeDetails {
        solana_fee::calculate_fee_details(
            message,
            lamports_per_signature == 0, // has_fee_waiver
            lamports_per_signature,
            prioritization_fee,
            FeeFeatures::from(feature_set),
        )
    }
}
