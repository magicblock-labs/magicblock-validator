# Test Manual

This workspace contains manual integration tests for the Magicblock validator that require external dependencies or manual setup steps that prevent full automation.

## Tests

### [helius-laser](helius-laser/)

Tests the validator's laser gRPC client integration with Helius devnet. This test validates that the validator can properly clone accounts from remote clusters and maintain synchronized state.

**Requirements**: Helius API key, Solana devnet keypair with SOL

**Run with**: `make manual-test-laser`

See the [helius-laser README](helius-laser/README.md) for detailed setup and usage instructions.

## Why Manual Tests?

These tests cannot be fully automated because they require:

- External API keys (Helius, etc.)
- Real blockchain accounts with funds
- Manual setup steps
- Network connectivity to external services

They are designed to validate real-world integration scenarios that unit/integration tests cannot cover.
