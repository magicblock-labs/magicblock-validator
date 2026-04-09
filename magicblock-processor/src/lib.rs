#![allow(deprecated)]

use std::sync::Arc;

use agave_syscalls::{
    create_program_runtime_environment_v1,
    create_program_runtime_environment_v2,
};
use magicblock_accounts_db::{traits::AccountsBank, AccountsDb};
use magicblock_core::link::blocks::BlockHash;
use solana_account::{AccountSharedData, WritableAccount};
use solana_feature_gate_interface::state::Feature;
use solana_feature_set::{
    curve25519_restrict_msm_length, curve25519_syscall_enabled,
    disable_rent_fees_collection, ed25519_program_enabled,
    enable_poseidon_syscall, enable_secp256r1_precompile,
    enable_transaction_loading_failure_fees, get_sysvar_syscall_enabled,
    secp256k1_program_enabled, FeatureSet,
};
use solana_program::{pubkey::Pubkey, rent::Rent};
use solana_program_runtime::{
    execution_budget::SVMTransactionExecutionBudget,
    loaded_programs::ProgramRuntimeEnvironments,
    solana_sbpf::program::BuiltinProgram,
};
use solana_sdk_ids::{
    ed25519_program, native_loader, secp256k1_program, secp256r1_program,
};
use solana_svm::transaction_processor::TransactionProcessingEnvironment;
use tracing::error;

/// Transaction processing environment plus the exact active Agave feature set.
pub struct SvmEnv {
    pub environment: TransactionProcessingEnvironment,
    pub feature_set: FeatureSet,
}

/// Initialize an SVM environment for transaction processing and retain the active feature set.
pub fn build_svm_env(
    accountsdb: &AccountsDb,
    blockhash: BlockHash,
    fee_per_signature: u64,
) -> SvmEnv {
    let mut feature_set = FeatureSet::default();

    // Activate features relevant to ER operations:
    // - Rent exemption for all regular accounts (disable collection).
    // - Curve25519 syscalls.
    // - Poseidon syscall.
    // - Fees for failed transaction loading (DoS mitigation).
    for id in [
        disable_rent_fees_collection::ID,
        curve25519_syscall_enabled::ID,
        curve25519_restrict_msm_length::ID,
        enable_poseidon_syscall::ID,
        enable_transaction_loading_failure_fees::ID,
        get_sysvar_syscall_enabled::ID,
        ed25519_program_enabled::ID,
        secp256k1_program_enabled::ID,
        enable_secp256r1_precompile::ID,
    ] {
        feature_set.activate(&id, 0);
    }

    // Persist active features to AccountsDb if they don't already exist.
    // This ensures programs checking for these features find them.
    for (id, &slot) in feature_set.active() {
        ensure_feature_account(accountsdb, id, Some(slot));
    }

    ensure_precompile_account(accountsdb, &ed25519_program::ID);
    ensure_precompile_account(accountsdb, &secp256k1_program::ID);
    ensure_precompile_account(accountsdb, &secp256r1_program::ID);
    ensure_builtin_accounts(accountsdb);

    let budget = SVMTransactionExecutionBudget::new_with_defaults(false);
    let runtime_features = feature_set.runtime_features();
    let runtime_v1 = create_program_runtime_environment_v1(
        &runtime_features,
        &budget,
        false,
        false,
    )
    .and_then(|mut runtime| {
        runtime.register_function(
            "sol_matmul_i8",
            syscalls::SyscallMatmulI8::vm,
        )?;
        Ok(runtime)
    })
    .map(Into::into)
    .unwrap_or_else(|_| {
        Arc::new(BuiltinProgram::new_loader(Default::default()))
    });
    let runtime_v2 = {
        let mut runtime = create_program_runtime_environment_v2(&budget, false);
        runtime
            .register_function("sol_matmul_i8", syscalls::SyscallMatmulI8::vm)
            .expect(
                "failed to register sol_matmul_i8 in runtime environment v2",
            );
        runtime.into()
    };
    let runtime_environments = ProgramRuntimeEnvironments {
        program_runtime_v1: runtime_v1,
        program_runtime_v2: runtime_v2,
    };

    let environment = TransactionProcessingEnvironment {
        blockhash,
        blockhash_lamports_per_signature: fee_per_signature,
        feature_set: runtime_features,
        epoch_total_stake: 0,
        program_runtime_environments_for_execution: runtime_environments
            .clone(),
        program_runtime_environments_for_deployment: runtime_environments,
        rent: Rent::default(),
    };

    SvmEnv {
        environment,
        feature_set,
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

    let feature = Feature { activated_at };
    let account = solana_feature_gate_interface::create_account(&feature, 1);
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

fn ensure_builtin_accounts(accountsdb: &AccountsDb) {
    for builtin in builtins::BUILTINS {
        if accountsdb.get_account(&builtin.program_id).is_some() {
            continue;
        }

        let mut account = AccountSharedData::new(1, 0, &native_loader::ID);
        account.set_executable(true);
        if let Err(err) =
            accountsdb.insert_account(&builtin.program_id, &account)
        {
            error!(
                "Failed to insert builtin account {}: {:?}",
                builtin.program_id, err
            );
        }
    }
}

mod builtins;
mod executor;
pub mod loader;
pub mod scheduler;
mod syscalls;
