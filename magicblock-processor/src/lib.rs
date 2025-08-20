use magicblock_accounts_db::AccountsDb;
use magicblock_core::link::blocks::BlockHash;
use solana_account::AccountSharedData;
use solana_feature_set::{
    curve25519_restrict_msm_length, curve25519_syscall_enabled,
    disable_rent_fees_collection, FeatureSet,
};
use solana_program::feature;
use solana_rent_collector::RentCollector;
use solana_svm::transaction_processor::TransactionProcessingEnvironment;

type WorkerId = u8;

/// Initialize an SVM enviroment for transaction processing
pub fn build_svm_env(
    accountsdb: &AccountsDb,
    blockhash: BlockHash,
    fee_per_signature: u64,
) -> TransactionProcessingEnvironment<'static> {
    let mut featureset = FeatureSet::default();

    // Activate list of features which are relevant to ER operations
    featureset.activate(&disable_rent_fees_collection::ID, 0);
    featureset.activate(&curve25519_syscall_enabled::ID, 0);
    featureset.activate(&curve25519_restrict_msm_length::ID, 0);

    let active = featureset.active.iter().map(|(k, &v)| (k, Some(v)));
    for (feature_id, activated_at) in active {
        // Skip if the feature account already exists
        if accountsdb.get_account(feature_id).is_some() {
            continue;
        }
        // Create a Feature struct with activated_at set to slot 0
        let f = feature::Feature { activated_at };
        let Ok(account) = AccountSharedData::new_data(1, &f, &feature::id())
        else {
            continue;
        };
        accountsdb.insert_account(feature_id, &account);
    }

    // We have a static rent which is setup once at startup,
    // and never changes afterwards. For now we use the same
    // values as the vanila solana validator (default())
    let rent_collector = Box::leak(Box::new(RentCollector::default()));

    TransactionProcessingEnvironment {
        blockhash,
        blockhash_lamports_per_signature: fee_per_signature,
        feature_set: featureset.into(),
        fee_lamports_per_signature: fee_per_signature,
        rent_collector: Some(rent_collector),
        epoch_total_stake: 0,
    }
}

mod builtins;
mod executor;
pub mod loader;
pub mod scheduler;
