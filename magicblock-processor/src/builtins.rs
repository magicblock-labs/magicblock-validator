use magicblock_program::magicblock_processor;
use solana_program_runtime::invoke_context::BuiltinFunctionWithContext;
use solana_pubkey::Pubkey;
use solana_sdk_ids::{bpf_loader_upgradeable, compute_budget};

pub struct BuiltinPrototype {
    pub program_id: Pubkey,
    pub name: &'static str,
    pub entrypoint: BuiltinFunctionWithContext,
}

/// We support and load the following builtin programs at startup:
///
/// - `system_program`
/// - `solana_bpf_loader_upgradeable_program`
/// - `compute_budget_program"
/// - `address_lookup_table_program`
/// - `magicblock_program` which supports account mutations, etc.
///
/// We don't support the following builtin programs:
///
/// - `vote_program` since we have no votes
/// - `stake_program` since we don't support staking in our validator
/// - `config_program` since we don't support configuration (_Add configuration data to the chain and the
///   list of public keys that are permitted to modify it_)
/// - `solana_bpf_loader_deprecated_program` because it's deprecated
/// - `solana_bpf_loader_program` since we use the `solana_bpf_loader_upgradeable_program` instead
/// - `zk_token_proof_program` it's behind a feature flag (`feature_set::zk_token_sdk_enabled`) in
///   the solana validator and we don't support it yet
/// - `solana_sdk::loader_v4` it's behind a feature flag (`feature_set::enable_program_runtime_v2_and_loader_v4`) in the solana
///   validator and we don't support it yet
///
/// See: solana repo - runtime/src/builtins.rs
pub static BUILTINS: &[BuiltinPrototype] = &[
    BuiltinPrototype {
        program_id: solana_system_program::id(),
        name: "system_program",
        entrypoint: solana_system_program::system_processor::Entrypoint::vm,
    },
    BuiltinPrototype {
        program_id: bpf_loader_upgradeable::id(),
        name: "solana_bpf_loader_upgradeable_program",
        entrypoint: solana_bpf_loader_program::Entrypoint::vm,
    },
    BuiltinPrototype {
        program_id: solana_sdk_ids::loader_v4::id(),
        name: "solana_loader_v4_program",
        entrypoint: solana_loader_v4_program::Entrypoint::vm,
    },
    BuiltinPrototype {
        program_id: magicblock_program::id(),
        name: "magicblock_program",
        entrypoint: magicblock_processor::Entrypoint::vm,
    },
    BuiltinPrototype {
        program_id: compute_budget::id(),
        name: "compute_budget_program",
        entrypoint: solana_compute_budget_program::Entrypoint::vm,
    },
];
