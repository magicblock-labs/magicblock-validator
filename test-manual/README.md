# Manual integration tests

Tests for integrations that need external services, credentials, and funded
accounts. This is a separate Cargo workspace.

## Available tests

[Helius Laser](helius-laser/README.md) exercises base-chain account cloning and
subscription updates using Helius or Triton devnet services.

Read its prerequisites and runner limitations before use. The runner is
`make test-laser` from this directory, but it needs updating for the current
validator CLI and configuration.

[Back to workspace](../README.md)
