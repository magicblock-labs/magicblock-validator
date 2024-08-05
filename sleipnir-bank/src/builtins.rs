// NOTE: copied from runtime/src/builtins.rs
use solana_program_runtime::invoke_context::BuiltinFunctionWithContext;
use solana_sdk::{
    address_lookup_table, bpf_loader_upgradeable, compute_budget,
    pubkey::Pubkey,
};

pub struct BuiltinPrototype {
    pub feature_id: Option<Pubkey>,
    pub program_id: Pubkey,
    pub name: &'static str,
    pub entrypoint: BuiltinFunctionWithContext,
}

impl std::fmt::Debug for BuiltinPrototype {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let mut builder = f.debug_struct("BuiltinPrototype");
        builder.field("program_id", &self.program_id);
        builder.field("name", &self.name);
        builder.field("feature_id", &self.feature_id);
        builder.finish()
    }
}

#[cfg(RUSTC_WITH_SPECIALIZATION)]
impl solana_frozen_abi::abi_example::AbiExample for BuiltinPrototype {
    fn example() -> Self {
        // BuiltinPrototype isn't serializable by definition.
        solana_program_runtime::declare_process_instruction!(
            MockBuiltin,
            0,
            |_invoke_context| {
                // Do nothing
                Ok(())
            }
        );
        Self {
            feature_id: None,
            program_id: Pubkey::default(),
            name: "",
            entrypoint: MockBuiltin::vm,
        }
    }
}

/// We support and load the following builtin programs at startup:
///
/// - `system_program`
/// - `solana_bpf_loader_upgradeable_program`
/// - `compute_budget_program"t`
/// - `address_lookup_table_program`
/// - `sleipnir_program` which supports account mutations, etc.
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
        feature_id: None,
        program_id: solana_system_program::id(),
        name: "system_program",
        entrypoint: solana_system_program::system_processor::Entrypoint::vm,
    },
    BuiltinPrototype {
        feature_id: None,
        program_id: bpf_loader_upgradeable::id(),
        name: "solana_bpf_loader_upgradeable_program",
        entrypoint: solana_bpf_loader_program::Entrypoint::vm,
    },
    BuiltinPrototype {
        feature_id: None,
        program_id: sleipnir_program::id(),
        name: "sleipnir_program",
        entrypoint: sleipnir_program::sleipnir_processor::Entrypoint::vm,
    },
    BuiltinPrototype {
        feature_id: None,
        program_id: compute_budget::id(),
        name: "compute_budget_program",
        entrypoint: solana_compute_budget_program::Entrypoint::vm,
    },
    BuiltinPrototype {
        feature_id: None,
        program_id: address_lookup_table::program::id(),
        name: "address_lookup_table_program",
        entrypoint:
            solana_address_lookup_table_program::processor::Entrypoint::vm,
    },
];
