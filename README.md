## MagicBlock's Ephemeral Validator

## Research

### Solana Validator

- [crates](https://miro.com/app/board/uXjVNt95ws4=/) (`./sh/depgraph solana-runtime`)

#### SVM

- [docs](https://docs.rs/solana-program-runtime/latest/solana_svm/) (not yet published)
- [recently separated](https://github.com/solana-labs/solana/pull/35119) from other parts
  (mainly the solana-runtime crate)

#### Solana Program Runtime

- [docs](https://docs.rs/solana-program-runtime/latest/solana_program_runtime/)

#### SDK

- includes C bindings as well, for programs written in in C

**Sub Crates**

These crates just happen to be located below the `sdk` directory, but are not actually
dependencies of the `sdk` crate.
Example: `solana-program = { path = "sdk/program", version = "=1.18.0" }`

- **bpf** compiler-builtins for BPF
- **sbpf** compiler-builtins for SBPF

- **gen-headers** binary to generate C headers

- **cargo-build-bpf** binary for building BPF programs
- **cargo-build-sbf** binary for building SBF programs
- **cargo-test-bpf** binary for testing BPF programs
- **cargo-test-sbf** binary for testing SBF programs

- **macro** defines macros, i.e.`declare_id!`j
- **program** various core features
  - builtin programs, i.e. _system_, _bpf-loader_
  - serialization tooling (borsh)
  - vars, i.e. _sys-var_, _rent_
  - data structs like _account-info_
  - compute units
  - syscalls
  - instruction
  - keccak hasher
  - program_memory
  - clock including constants to define things like _DEFAULT_SLOTS_PER_EPOCH_
  - many more

**SDK Itself**

- large collection of crates that need to be accessed by lots of other crates, thus it serves
  not only as an SDK, but also as a core crate
- `exit.rs` is a good example of a core feature which allows services to register to be called
  back when the validator is shutting down
