# Helius Laser integration test

Exercises account cloning and subscription updates against devnet. The test
makes transfers on the base chain and checks that the local validator observes
the resulting account state.

## Prerequisites

- Rust and the Solana CLI.
- A configured Solana keypair with enough devnet SOL for transfers and fees.
- `HELIUS_API_KEY` for devnet RPC access.
- Optionally, `TRITON_API_KEY` to use Triton for streaming. Helius credentials
  are still required by the runner.

The runner attempts an airdrop, but funding must be available even if the
airdrop fails.

## Runner status

The [Makefile](../Makefile) provides `make test-laser`, run from
`test-manual/`. **It needs updating before use with the current validator.**
It passes a positional configuration path instead of `--config`, and its
[templates](configs/) use the old storage configuration layout.

The runner's startup wait has no timeout, and its final completion message is
not proof that assertions passed. Do not use it as an automated pass/fail gate.

For manual setup, use the current
[leader configuration](../../config.example.toml) and
[launch instructions](../../bins/magicblock-validator/README.md).
The [step-by-step scripts](sh/) show the intended setup and test sequence, but
share the runner's outdated startup assumptions.

[Back to manual tests](../README.md)
