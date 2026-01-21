# Test Manual

This workspace contains manual integration tests for the Magicblock validator that require external dependencies or manual setup steps that prevent full automation.

## Tests

### [grpc-account-tracking](grpc-account-tracking/)

Tests the validator's gRPC account tracking integration with Helius devnet. This test validates that the validator can properly clone accounts from remote clusters and maintain synchronized state.

**Requirements**: Helius API key, Solana devnet keypair with SOL

**Run with**: `make test-grpc-account-tracking`

See the [grpc-account-tracking README](grpc-account-tracking/README.md) for detailed setup and usage instructions.

## Why Manual Tests?

These tests cannot be fully automated because they require:

- External API keys (Helius, etc.)
- Real blockchain accounts with funds
- Manual setup steps
- Network connectivity to external services

They are designed to validate real-world integration scenarios that unit/integration tests cannot cover.
