use magicblock_program::magicblock_processor;
use solana_program_runtime::invoke_context::BuiltinFunctionWithContext;
use solana_pubkey::Pubkey;
use solana_sdk_ids::{
    bpf_loader_upgradeable, compute_budget, loader_v4, system_program,
};

/// Represents a builtin program to be registered with the SVM.
pub struct Builtin {
    pub program_id: Pubkey,
    pub name: &'static str,
    pub entrypoint: BuiltinFunctionWithContext,
}

/// The set of builtin programs loaded at startup.
///
/// **Supported:**
/// - `system_program`: Core system account management.
/// - `bpf_loader_upgradeable`: Loads upgradeable BPF programs.
/// - `loader_v4`: Loads V4 programs.
/// - `compute_budget`: Manages transaction compute units.
/// - `magicblock_program`: Validator-specific logic (e.g., account mutations).
///
/// **Explicitly Unsupported:**
/// - `vote_program`, `stake_program`: Consensus and staking are not supported.
/// - `config_program`: On-chain configuration is not supported.
/// - `bpf_loader_deprecated`, `bpf_loader`: Superseded by `upgradeable`.
/// - `zk_token_proof_program`: ZK Token SDK features are not yet enabled.
pub static BUILTINS: &[Builtin] = &[
    Builtin {
        program_id: system_program::ID,
        name: "system_program",
        entrypoint: solana_system_program::system_processor::Entrypoint::vm,
    },
    Builtin {
        program_id: bpf_loader_upgradeable::ID,
        name: "solana_bpf_loader_upgradeable_program",
        entrypoint: solana_bpf_loader_program::Entrypoint::vm,
    },
    Builtin {
        program_id: loader_v4::ID,
        name: "solana_loader_v4_program",
        entrypoint: solana_loader_v4_program::Entrypoint::vm,
    },
    Builtin {
        program_id: magicblock_program::ID,
        name: "magicblock_program",
        entrypoint: magicblock_processor::Entrypoint::vm,
    },
    Builtin {
        program_id: compute_budget::ID,
        name: "compute_budget_program",
        entrypoint: solana_compute_budget_program::Entrypoint::vm,
    },
];
