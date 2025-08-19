use magicblock_core::link::blocks::BlockHash;
use solana_feature_set::FeatureSet;
use solana_rent_collector::RentCollector;
use solana_svm::transaction_processor::TransactionProcessingEnvironment;

type WorkerId = u8;

mod builtins;
mod executor;
pub mod scheduler;

/// Initialize an SVM enviroment for transaction processing
pub fn build_svm_env(
    blockhash: BlockHash,
    fee_per_signature: u64,
    feature_set: FeatureSet,
) -> TransactionProcessingEnvironment<'static> {
    // We have a static rent which is setup once at startup,
    // and never changes afterwards. For now we use the same
    // values as the vanila solana validator (default())
    let rent_collector = Box::leak(Box::new(RentCollector::default()));

    TransactionProcessingEnvironment {
        blockhash,
        blockhash_lamports_per_signature: fee_per_signature,
        feature_set: feature_set.into(),
        fee_lamports_per_signature: fee_per_signature,
        rent_collector: Some(rent_collector),
        epoch_total_stake: 0,
    }
}
