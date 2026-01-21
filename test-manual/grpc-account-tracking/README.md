# gRPC Account Tracking Test

## What does it do

This test validates the validator's gRPC account tracking integration with Helius or Triton devnet.
It performs a series of account operations and verifies that the local validator correctly
clones and tracks account states from the remote devnet cluster.

The test:

1. Sets up a local validator configured to use Helius/Triton devnet as remote cluster
2. Performs SOL transfers between accounts on the remote cluster
3. Clones the involved accounts to the local validator
4. Does more transfers to trigger account state updates
5. Verifies that account states between Helius/Triton devnet and local validator match exactly

## Why can it not be fully automated

The test cannot be fully automated because it requires:

- Access to a Helius or Triton API key for connecting to their devnet RPC and laser/yellowstone endpoints
- A Solana keypair on devnet with sufficient SOL for transaction fees
- Manual airdrop of SOL to the test account before running

These external dependencies and manual setup steps prevent full automation.

## How to run it

1.a. Ensure you have a Helius API key set in the `HELIUS_API_KEY` environment variable:
   ```bash
   export HELIUS_API_KEY=your_helius_api_key_here
   ```
1.b. Alternatively, for Triton, set the `TRITON_API_KEY` environment variable:
   ```bash
   export TRITON_API_KEY=your_triton_api_key_here
   ```

2. Run the test from the `test-manual` directory:
    ```bash
    make test-grpc-account-tracking
    ```
That last step will ensure the following:

1. airdrop some SOL to your globally configured keypair
2. start a local validator in ephemeral mode after creating a
   temporary config which uses either a Helius or Triton GRPC endpoint
3. run the test script

The test script will:

- Start a local validator in ephemeral mode
- Perform account operations on devnet
- Clone accounts to the local validator
- Perform more account operations on devnet
- Compare account states between sources
- Display validator logs for verification
- Clean up the validator process

If successful, you'll see account cloning and subscription messages in the validator logs, followed by confirmation that all account states match.

## Run Steps in Isolation

Inside `test-manual/grpc-account-tracking/sh/` you can find bash scripts which you can run in order to
perform the above step by step.