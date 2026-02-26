# MagicBlock Ephemeral Validator

Solana-based ephemeral validator with delegation and commit/undelegation lifecycle.

## Build & Test

- **Build**: `cargo build` (root Cargo.toml)
- **Lint**: `make lint`
- **Format**: `make fmt`
- **Unit tests**: `cargo test -p <crate-name>` from repo root
- **Integration tests**: separate workspace at `test-integration/` with its own Cargo.toml, Cargo.lock, and Makefile
  - Build: `cd test-integration && cargo check`
  - Run: `cd test-integration && make test`
- **Formatting**: `max_width = 80`, edition 2021 (see `rustfmt.toml`)

## Project Structure

- `magicblock-validator/` — main validator binary
- `magicblock-committor-service/` — commit/finalize/undelegate task orchestration
- `magicblock-committor-program/` — on-chain program for buffer management (chunks, PDAs)
- `programs/magicblock/` — on-chain MagicBlock program (scheduling, intent processing)
- `magicblock-accounts*/` — account storage and cloning
- `magicblock-rpc-client/` — RPC client utilities
- `magicblock-metrics/` — metrics and instrumentation (`LabelValue` trait)
- `test-integration/` — integration test workspace (separate Cargo workspace, separate Makefile)
- `tools/` — CLI utilities (genx, keypair-base58, ledger-stats, tui-client)

## Code Style

- Keep match arms clean: prefer matching enum variants directly over guard clauses
- Move domain logic into the type's own `impl` block, keep delegation enums thin
- Avoid over-abstracting: three similar lines are better than a premature helper
- Use `From` impls for enum wrapping conversions
- Return iterators (`impl Iterator<Item = T>`) over collected `Vec<T>` where practical
