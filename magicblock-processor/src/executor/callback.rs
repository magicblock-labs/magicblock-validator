use solana_account::{AccountSharedData, WritableAccount};
use solana_feature_set::FeatureSet;
use solana_fee::FeeFeatures;
use solana_fee_structure::FeeDetails;
use solana_pubkey::Pubkey;
use solana_sdk_ids::native_loader;
use solana_svm::transaction_processing_callback::TransactionProcessingCallback;
use solana_svm_transaction::svm_message::SVMMessage;

/// Required implementation to use the executor within the SVM
impl TransactionProcessingCallback for super::TransactionExecutor {
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

    /// Add a builtin program account
    fn add_builtin_account(&self, name: &str, program_id: &Pubkey) {
        if self.accountsdb.contains_account(program_id) {
            return;
        }

        // Add a bogus executable builtin account, which will be loaded and ignored.
        let mut account =
            AccountSharedData::new(1, name.len(), &native_loader::id());
        account.set_data_from_slice(name.as_bytes());
        account.set_executable(true);
        self.accountsdb.insert_account(program_id, &account);
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
            lamports_per_signature == 0,
            lamports_per_signature,
            prioritization_fee,
            FeeFeatures::from(feature_set),
        )
    }
}
