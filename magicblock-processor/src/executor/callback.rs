use magicblock_accounts_db::traits::AccountsBank;
use solana_account::AccountSharedData;
use solana_precompile_error::PrecompileError;
use solana_pubkey::Pubkey;
use solana_svm::transaction_processing_callback::TransactionProcessingCallback;
use solana_svm_callback::InvokeContextCallback;

use super::TransactionExecutor;

impl InvokeContextCallback for TransactionExecutor {
    fn is_precompile(&self, program_id: &Pubkey) -> bool {
        agave_precompiles::is_precompile(program_id, |feature_id| {
            self.feature_set.is_active(feature_id)
        })
    }

    fn process_precompile(
        &self,
        program_id: &Pubkey,
        data: &[u8],
        instruction_datas: Vec<&[u8]>,
    ) -> Result<(), PrecompileError> {
        let Some(precompile) =
            agave_precompiles::get_precompile(program_id, |feature_id| {
                self.feature_set.is_active(feature_id)
            })
        else {
            return Err(PrecompileError::InvalidPublicKey);
        };

        precompile.verify(data, &instruction_datas, &self.feature_set)
    }
}

impl TransactionProcessingCallback for TransactionExecutor {
    fn get_account_shared_data(
        &self,
        pubkey: &Pubkey,
    ) -> Option<(AccountSharedData, u64)> {
        self.accountsdb
            .get_account(pubkey)
            .map(|account| (account, self.accountsdb.slot()))
    }
}
