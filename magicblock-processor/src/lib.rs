use magicblock_accounts_db::{traits::AccountsBank, AccountsDb};
use magicblock_core::link::blocks::BlockHash;
use solana_account::{AccountSharedData, WritableAccount};
use solana_feature_set::FeatureSet;
use solana_program::{feature, pubkey::Pubkey};
#[allow(deprecated)]
use solana_rent_collector::RentCollector;
use solana_sdk_ids::{
    ed25519_program, native_loader, secp256k1_program, secp256r1_program,
};
use solana_svm::transaction_processor::TransactionProcessingEnvironment;
use tracing::error;

/// Initialize an SVM environment for transaction processing.
pub fn build_svm_env(
    accountsdb: &AccountsDb,
    blockhash: BlockHash,
    fee_per_signature: u64,
) -> TransactionProcessingEnvironment<'static> {
    // All features enabled (broadest compatibility; not mainnet-parity).
    let feature_set = FeatureSet::all_enabled();

    // Persist active features to AccountsDb if they don't already exist.
    // This ensures programs checking for these features find them.
    for (id, &slot) in &feature_set.active {
        ensure_feature_account(accountsdb, id, Some(slot));
    }

    ensure_precompile_account(accountsdb, &ed25519_program::ID);
    ensure_precompile_account(accountsdb, &secp256k1_program::ID);
    ensure_precompile_account(accountsdb, &secp256r1_program::ID);

    // Initialize static RentCollector (leaked for 'static lifetime as it never changes).
    #[allow(deprecated)]
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

/// Helper to create and insert a feature account if it is missing.
fn ensure_feature_account(
    accountsdb: &AccountsDb,
    id: &Pubkey,
    activated_at: Option<u64>,
) {
    if accountsdb.get_account(id).is_some() {
        return;
    }

    let feature = feature::Feature { activated_at };
    let Ok(account) = AccountSharedData::new_data(1, &feature, &feature::id())
    else {
        return;
    };
    let _ = accountsdb.insert_account(id, &account);
}

fn ensure_precompile_account(accountsdb: &AccountsDb, id: &Pubkey) {
    if accountsdb.get_account(id).is_some() {
        return;
    }

    let mut account = AccountSharedData::new(1, 0, &native_loader::ID);
    account.set_executable(true);
    if let Err(e) = accountsdb.insert_account(id, &account) {
        error!("Failed to insert precompile account {}: {:?}", id, e);
    }
}

mod builtins;
mod executor;
pub mod loader;
pub mod scheduler;
