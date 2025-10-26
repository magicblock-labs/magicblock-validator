# Helius Laser Test

## What does it do

This test validates the validator's laser gRPC client integration with Helius devnet. It performs a series of account operations and verifies that the local validator correctly clones and tracks account states from the remote Helius cluster.

The test:

1. Sets up a local validator configured to use Helius devnet as remote cluster
2. Performs SOL transfers between accounts on the remote cluster
3. Clones the involved accounts to the local validator
4. Does more transfers to trigger account state updates
5. Verifies that account states between Helius devnet and local validator match exactly

## Why can it not be fully automated

The test cannot be fully automated because it requires:

- Access to a Helius API key for connecting to their devnet RPC and laser endpoints
- A Solana keypair on devnet with sufficient SOL for transaction fees
- Manual airdrop of SOL to the test account before running

These external dependencies and manual setup steps prevent full automation.

## How to run it

1. Ensure you have a Helius API key set in the `HELIUS_API_KEY` environment variable:
   ```bash
   export HELIUS_API_KEY=your_helius_api_key_here
   ```

2. Ensure your Solana CLI is configured for devnet:
   ```bash
   solana config set --url devnet
   ```

3. Airdrop some SOL to your configured keypair:
   ```bash
   solana airdrop 1
   ```

4. Run the test from the `test-manual` directory:
   ```bash
   make manual-test-laser
   ```

The test will:

- Start a local validator in ephemeral mode
- Perform account operations on devnet
- Clone accounts to the local validator
- Perform more account operations on devnet
- Compare account states between sources
- Display validator logs for verification
- Clean up the validator process

If successful, you'll see account cloning and subscription messages in the validator logs, followed by confirmation that all account states match.
